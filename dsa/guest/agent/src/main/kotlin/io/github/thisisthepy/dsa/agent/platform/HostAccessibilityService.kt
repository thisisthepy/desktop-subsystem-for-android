package io.github.thisisthepy.dsa.agent.platform

import android.accessibilityservice.AccessibilityService
import android.content.ComponentName
import android.content.Context
import android.graphics.Rect
import android.os.Bundle
import android.provider.Settings
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityWindowInfo
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilityExport
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilitySource
import io.github.thisisthepy.dsa.agent.accessibility.NodeHandle
import io.github.thisisthepy.dsa.agent.accessibility.NodeInfo
import io.github.thisisthepy.dsa.agent.accessibility.RangeInfo
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.NodeAction

/**
 * The agent's accessibility service: it reads the nodes of the tasks a host accessibility
 * client watches. It is enabled only while the host watches a task ([AndroidAccessibilitySource])
 * and shows nothing. Its configuration (res/xml/host_accessibility_service.xml) asks for
 * window content and the interactive windows of every task, and for no gestures, no touch
 * exploration and no key filtering, so the user's input reaches apps as it does without it.
 */
class HostAccessibilityService : AccessibilityService() {
    override fun onServiceConnected() {
        instance = this
        onChange?.invoke()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        onChange?.invoke()
    }

    override fun onInterrupt() = Unit

    override fun onDestroy() {
        if (instance === this) instance = null
        super.onDestroy()
    }

    companion object {
        @Volatile
        var instance: HostAccessibilityService? = null
            private set

        /** Who hears that a window's content may have changed, while the export is on. */
        @Volatile
        var onChange: (() -> Unit)? = null
    }
}

/**
 * Turns the agent's accessibility service on and off through the enabled_accessibility_services
 * secure setting, which the agent may write as a platform app (WRITE_SECURE_SETTINGS), without
 * asking the user. Other services the user enabled stay as they are, and the setting that
 * says accessibility is on is cleared only when no service is left. The agent's input method
 * and home activity are other components and are not touched.
 */
class AndroidAccessibilitySource(private val context: Context, private val log: Log) : AccessibilitySource {
    private val component = ComponentName(context, HostAccessibilityService::class.java).flattenToString()

    override fun enable(onChange: () -> Unit) {
        HostAccessibilityService.onChange = onChange
        val resolver = context.contentResolver
        val enabled = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        Settings.Secure.putString(
            resolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
            AccessibilityExport.withService(enabled, component),
        )
        Settings.Secure.putInt(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 1)
    }

    override fun disable() {
        HostAccessibilityService.onChange = null
        val resolver = context.contentResolver
        val enabled = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        val rest = AccessibilityExport.withoutService(enabled, component)
        if (rest != enabled.orEmpty()) {
            Settings.Secure.putString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES, rest)
        }
        if (rest.isEmpty()) Settings.Secure.putInt(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 0)
        // A service the setting no longer names is unbound by the system; this makes sure.
        HostAccessibilityService.instance?.disableSelf()
    }

    override fun roots(taskId: Int): List<NodeHandle>? {
        val service = HostAccessibilityService.instance ?: return null
        val windows = service.windows ?: return null
        return windows
            .filter { it.type == AccessibilityWindowInfo.TYPE_APPLICATION && taskOf(it) == taskId }
            .sortedBy { it.layer }
            .mapNotNull { window -> window.root?.let(::AndroidNode) }
    }

    /**
     * The task a window belongs to. AccessibilityWindowInfo.getTaskId() is hidden in the SDK,
     * but the window manager fills it for every app window (AccessibilityWindowsPopulator) and
     * a platform app may call it. Read by reflection; a framework without it gives no task.
     */
    private fun taskOf(window: AccessibilityWindowInfo): Int? = try {
        getTaskId.invoke(window) as Int
    } catch (e: ReflectiveOperationException) {
        if (!warned) {
            warned = true
            log.warn("the framework does not say which task a window belongs to; no accessibility tree can be exported", e)
        }
        null
    }

    private var warned = false

    private companion object {
        val getTaskId: java.lang.reflect.Method by lazy { AccessibilityWindowInfo::class.java.getMethod("getTaskId") }
    }
}

/** One node of a window, read through the framework's AccessibilityNodeInfo. */
private class AndroidNode(private val node: AccessibilityNodeInfo) : NodeHandle {
    // AccessibilityNodeInfo is equal for the same view of the same window, however often read.
    override val key: Any = node

    override fun info(): NodeInfo {
        val bounds = Rect().also(node::getBoundsInScreen)
        val ids = node.actionList.map { it.id }.toSet()
        return NodeInfo(
            className = node.className?.toString().orEmpty(),
            text = node.text?.toString().orEmpty(),
            description = node.contentDescription?.toString().orEmpty(),
            hint = node.hintText?.toString().orEmpty(),
            viewId = node.viewIdResourceName.orEmpty(),
            stateDescription = node.stateDescription?.toString().orEmpty(),
            bounds = TaskBounds(bounds.left, bounds.top, bounds.right, bounds.bottom),
            visible = node.isVisibleToUser,
            checkable = node.isCheckable,
            checked = node.isChecked,
            selected = node.isSelected,
            enabled = node.isEnabled,
            focusable = node.isFocusable,
            focused = node.isFocused,
            scrollable = node.isScrollable,
            clickable = node.isClickable,
            longClickable = node.isLongClickable,
            editable = node.isEditable,
            password = node.isPassword,
            heading = node.isHeading,
            range = node.rangeInfo?.let { RangeInfo(it.min, it.max, it.current) },
            actions = ACTIONS.filter { (_, id) -> id in ids }.map { (action, _) -> action },
        )
    }

    override fun children(): List<NodeHandle> = (0 until node.childCount).mapNotNull { node.getChild(it)?.let(::AndroidNode) }

    override fun perform(action: NodeAction, text: String): Boolean {
        val id = ACTIONS.firstOrNull { it.first == action }?.second ?: return false
        val arguments = if (action == NodeAction.NODE_ACTION_SET_TEXT) {
            Bundle().apply { putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text) }
        } else {
            null
        }
        // Read again, so the action goes to the node as it is now.
        node.refresh()
        return node.performAction(id, arguments)
    }

    private companion object {
        val ACTIONS = listOf(
            NodeAction.NODE_ACTION_CLICK to AccessibilityNodeInfo.ACTION_CLICK,
            NodeAction.NODE_ACTION_LONG_CLICK to AccessibilityNodeInfo.ACTION_LONG_CLICK,
            NodeAction.NODE_ACTION_SET_TEXT to AccessibilityNodeInfo.ACTION_SET_TEXT,
            NodeAction.NODE_ACTION_SCROLL_FORWARD to AccessibilityNodeInfo.ACTION_SCROLL_FORWARD,
            NodeAction.NODE_ACTION_SCROLL_BACKWARD to AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD,
            NodeAction.NODE_ACTION_FOCUS to AccessibilityNodeInfo.ACTION_FOCUS,
            NodeAction.NODE_ACTION_ACCESSIBILITY_FOCUS to AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS,
            NodeAction.NODE_ACTION_SELECT to AccessibilityNodeInfo.ACTION_SELECT,
        )
    }
}
