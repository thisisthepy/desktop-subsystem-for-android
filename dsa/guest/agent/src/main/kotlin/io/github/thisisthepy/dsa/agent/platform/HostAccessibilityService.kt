package io.github.thisisthepy.dsa.agent.platform

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.AccessibilityServiceInfo
import android.content.ComponentName
import android.content.Context
import android.graphics.Rect
import android.os.Bundle
import android.provider.Settings
import android.text.Spanned
import android.text.style.ClickableSpan
import android.text.style.URLSpan
import android.view.View
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityManager
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityNodeInfo.AccessibilityAction
import android.view.accessibility.AccessibilityWindowInfo
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilityChange
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilityExport
import io.github.thisisthepy.dsa.agent.accessibility.AccessibilitySource
import io.github.thisisthepy.dsa.agent.accessibility.ActionArguments
import io.github.thisisthepy.dsa.agent.accessibility.CollectionInfo
import io.github.thisisthepy.dsa.agent.accessibility.CollectionItemInfo
import io.github.thisisthepy.dsa.agent.accessibility.CustomAction
import io.github.thisisthepy.dsa.agent.accessibility.NodeHandle
import io.github.thisisthepy.dsa.agent.accessibility.NodeInfo
import io.github.thisisthepy.dsa.agent.accessibility.RangeInfo
import io.github.thisisthepy.dsa.agent.accessibility.TextLink
import io.github.thisisthepy.dsa.agent.accessibility.WindowInfo
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.NodeAction

/**
 * The agent's accessibility service: it reads the nodes of the tasks a host accessibility
 * client watches. It is enabled only while the host watches a task ([AndroidAccessibilitySource])
 * and shows nothing. Its configuration (res/xml/host_accessibility_service.xml) asks for
 * window content and the interactive windows of every task, and for no gestures, no touch
 * exploration and no key filtering, so the user's input reaches apps as it does without it.
 *
 * Without a screen reader it takes the event kinds the tree needs, batched by Android for
 * 100 ms, and tells the export only that something changed, as it always has. While a screen
 * reader listens ([listening]) it takes every event at once and the kinds only a screen
 * reader needs (selection, the accessibility focus, announcements), and tells the export
 * which task and which node each event is about, so the export reads only that.
 */
class HostAccessibilityService : AccessibilityService() {
    override fun onServiceConnected() {
        instance = this
        AndroidLog.info("Android bound the agent's accessibility service")
        takeEvents(listening)
        onEvent?.invoke(AccessibilityChange.ANY)
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        val onEvent = onEvent ?: return
        if (event == null || !listening) {
            onEvent(AccessibilityChange.ANY)
            return
        }
        onEvent(change(event) ?: return)
    }

    /**
     * What the event says. Read here: Android reuses the event once this returns. This runs on
     * the agent's UI thread, which also takes HomeActivity's input, so it asks Android nothing.
     * The window's task and the event's node are found by asking Android, which waits on the
     * app that owns the window, up to five seconds when that app is closing; that is left to
     * [AccessibilityChange.resolve], which the export runs on its reader thread.
     */
    private fun change(event: AccessibilityEvent): AccessibilityChange? {
        val text = event.text.joinToString(" ") { it?.toString().orEmpty() }.trim()
        val packageName = event.packageName?.toString().orEmpty()
        return when (event.eventType) {
            AccessibilityEvent.TYPE_ANNOUNCEMENT -> AccessibilityChange(AccessibilityChange.Kind.ANNOUNCEMENT, text = text, packageName = packageName)
            AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED ->
                // A notification reaches the host by its own way; a toast only this one.
                if (event.className?.toString()?.contains("Toast") == true) {
                    AccessibilityChange(AccessibilityChange.Kind.TOAST, text = text, packageName = packageName)
                } else {
                    null
                }
            else -> {
                // The agent's own windows (its home, its splash screens and the toasts it draws
                // for apps) belong to no task the host watches, and asking about them would
                // wait on the agent's own UI thread.
                if (packageName == this.packageName) return null
                val kind = when (event.eventType) {
                    AccessibilityEvent.TYPE_VIEW_FOCUSED,
                    AccessibilityEvent.TYPE_VIEW_SELECTED,
                    AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUSED,
                    AccessibilityEvent.TYPE_VIEW_TEXT_SELECTION_CHANGED,
                    -> AccessibilityChange.Kind.FOCUS
                    else -> AccessibilityChange.Kind.CONTENT
                }
                // A copy keeps what getSource() needs after Android reuses the event.
                val copy = AccessibilityEvent(event)
                val windowId = event.windowId
                AccessibilityChange(kind, packageName = packageName, resolve = { resolve(kind, windowId, copy) })
            }
        }
    }

    /** Finds the event's task and node. Asks Android, so it runs on the export's reader thread. */
    private fun resolve(kind: AccessibilityChange.Kind, windowId: Int, event: AccessibilityEvent): AccessibilityChange =
        AccessibilityChange(kind, taskOf(windowId), event.source?.let { AndroidNode(it, this) })

    /** The task of the window [windowId], from the windows Android keeps for the service. */
    private fun taskOf(windowId: Int): Int? {
        if (windowId < 0) return null
        val window = windows?.firstOrNull { it.id == windowId } ?: return null
        return taskOfWindow(window)
    }

    /** Takes the events and batching [listening] asks for. */
    fun takeEvents(listening: Boolean) {
        val info = serviceInfo ?: return
        info.eventTypes = if (listening) LISTENING_EVENTS else USUAL_EVENTS
        info.notificationTimeout = if (listening) 0 else USUAL_TIMEOUT_MILLIS
        serviceInfo = info
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

        /** Who hears what each event says, while the export is on. */
        @Volatile
        var onEvent: ((AccessibilityChange) -> Unit)? = null

        /** Whether a screen reader listens for a watched task. */
        @Volatile
        var listening = false

        /** The kinds the tree needs, as res/xml/host_accessibility_service.xml lists them. */
        const val USUAL_EVENTS = AccessibilityEvent.TYPE_WINDOW_STATE_CHANGED or
            AccessibilityEvent.TYPE_WINDOW_CONTENT_CHANGED or
            AccessibilityEvent.TYPE_WINDOWS_CHANGED or
            AccessibilityEvent.TYPE_VIEW_SCROLLED or
            AccessibilityEvent.TYPE_VIEW_FOCUSED or
            AccessibilityEvent.TYPE_VIEW_SELECTED or
            AccessibilityEvent.TYPE_VIEW_TEXT_CHANGED

        /** And what a screen reader needs besides. */
        const val LISTENING_EVENTS = USUAL_EVENTS or
            AccessibilityEvent.TYPE_VIEW_TEXT_SELECTION_CHANGED or
            AccessibilityEvent.TYPE_VIEW_ACCESSIBILITY_FOCUSED or
            AccessibilityEvent.TYPE_ANNOUNCEMENT or
            AccessibilityEvent.TYPE_NOTIFICATION_STATE_CHANGED

        /** How long Android batches the events of one change without a screen reader. */
        const val USUAL_TIMEOUT_MILLIS = 100L

        /**
         * The task a window belongs to. AccessibilityWindowInfo.getTaskId() is hidden in the
         * SDK, but the window manager fills it for every app window and a platform app may call
         * it. Read by reflection; a framework without it gives no task.
         */
        fun taskOfWindow(window: AccessibilityWindowInfo): Int? = try {
            getTaskId.invoke(window) as Int
        } catch (e: ReflectiveOperationException) {
            null
        }

        private val getTaskId: java.lang.reflect.Method by lazy { AccessibilityWindowInfo::class.java.getMethod("getTaskId") }
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

    override fun enable(onChange: () -> Unit) = enableEvents { onChange() }

    override fun enableEvents(onEvent: (AccessibilityChange) -> Unit) {
        HostAccessibilityService.onEvent = onEvent
        turnOn()
    }

    override fun setListening(listening: Boolean) {
        HostAccessibilityService.listening = listening
        HostAccessibilityService.instance?.takeEvents(listening)
    }

    private fun turnOn() {
        val resolver = context.contentResolver
        val enabled = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        val named = Settings.Secure.putString(
            resolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
            AccessibilityExport.withService(enabled, component),
        )
        val on = Settings.Secure.putInt(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 1)
        if (!named || !on) {
            log.warn("Android did not take the settings that turn the agent's accessibility service on (${describe()})")
        }
    }

    override val connected: Boolean
        get() = HostAccessibilityService.instance != null

    override fun describe(): String {
        val resolver = context.contentResolver
        val manager = context.getSystemService(AccessibilityManager::class.java)
        val own = ComponentName.unflattenFromString(component)
        val isOwn = { info: AccessibilityServiceInfo -> ComponentName.unflattenFromString(info.id) == own }
        val installed = manager?.installedAccessibilityServiceList.orEmpty().any(isOwn)
        val running = manager?.getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_ALL_MASK).orEmpty().any(isOwn)
        val services = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES).orEmpty()
        val on = Settings.Secure.getInt(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 0)
        return "bound in the agent: ${if (connected) "yes" else "no"}; " +
            "Android lists it as installed: ${if (installed) "yes" else "no"}, as enabled: ${if (running) "yes" else "no"}; " +
            "accessibility ${if (manager?.isEnabled == true) "on" else "off"}; " +
            "enabled_accessibility_services=\"$services\", accessibility_enabled=$on"
    }

    /**
     * Android binds a service when the setting that names it changes. Writing the same value
     * again changes nothing, so the service is taken out of the setting and put back.
     */
    override fun reconnect() {
        val resolver = context.contentResolver
        val enabled = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        Settings.Secure.putString(
            resolver,
            Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
            AccessibilityExport.withoutService(enabled, component),
        )
        turnOn()
    }

    override fun disable() {
        HostAccessibilityService.onEvent = null
        HostAccessibilityService.listening = false
        val resolver = context.contentResolver
        val enabled = Settings.Secure.getString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES)
        val rest = AccessibilityExport.withoutService(enabled, component)
        if (rest != enabled.orEmpty()) {
            Settings.Secure.putString(resolver, Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES, rest)
        }
        if (rest.isEmpty()) Settings.Secure.putInt(resolver, Settings.Secure.ACCESSIBILITY_ENABLED, 0)
        HostAccessibilityService.instance?.disableSelf()
    }

    override fun roots(taskId: Int): List<NodeHandle>? {
        val service = HostAccessibilityService.instance ?: return null
        val windows = service.windows ?: return null
        val mine = windows
            .filter { it.type == AccessibilityWindowInfo.TYPE_APPLICATION && taskOf(it) == taskId }
            .sortedBy { it.layer }
        return mine.mapIndexedNotNull { place, window ->
            window.root?.let { root ->
                val className = root.className?.toString().orEmpty()
                val popup = place > 0 && POPUP_CLASSES.any { className.contains(it) }
                // Every window over the task's first one is a dialog or a pop-up of it.
                val info = WindowInfo(
                    title = window.title?.toString().orEmpty().ifEmpty { root.paneTitle?.toString().orEmpty() },
                    modal = place > 0 && !popup,
                    popup = popup,
                )
                AndroidNode(root, service, if (place > 0 || info.title.isNotEmpty()) info else null)
            }
        }
    }

    private fun taskOf(window: AccessibilityWindowInfo): Int? {
        val task = HostAccessibilityService.taskOfWindow(window)
        if (task == null && !warned) {
            warned = true
            log.warn("the framework does not say which task a window belongs to; no accessibility tree can be exported")
        }
        return task
    }

    private var warned = false

    private companion object {
        /** The root classes of Android's pop-up windows: menus and drop-down lists. */
        val POPUP_CLASSES = listOf("PopupDecorView", "PopupWindow", "DropDown", "MenuPopup")
    }
}

/** One node of a window, read through the framework's AccessibilityNodeInfo. */
private class AndroidNode(
    private val node: AccessibilityNodeInfo,
    private val context: Context,
    private val window: WindowInfo? = null,
) : NodeHandle {
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
            accessibilityFocused = node.isAccessibilityFocused,
            selectionStart = node.textSelectionStart,
            selectionEnd = node.textSelectionEnd,
            headingLevel = headingLevel(),
            link = node.extras?.getCharSequence(ROLE_DESCRIPTION)?.toString()?.equals("link", ignoreCase = true) == true,
            collection = node.collectionInfo?.let { CollectionInfo(it.rowCount, it.columnCount, it.isHierarchical) },
            collectionItem = node.collectionItemInfo?.let { CollectionItemInfo(it.rowIndex, it.rowSpan, it.columnIndex, it.columnSpan, it.isHeading) },
            liveRegion = node.liveRegion,
            paneTitle = node.paneTitle?.toString().orEmpty(),
            customActions = node.actionList
                .filter { (it.id ushr 24) == APP_RESOURCES && !it.label.isNullOrBlank() }
                .map { CustomAction(it.id, it.label.toString()) },
            links = spans().map { (text, span) -> TextLink(text, (span as? URLSpan)?.url.orEmpty()) },
            window = window,
        )
    }

    /** A web page's heading says its level in its role description ("heading 2"). */
    private fun headingLevel(): Int {
        val role = node.extras?.getCharSequence(ROLE_DESCRIPTION)?.toString().orEmpty()
        return Regex("([1-9])").find(role)?.value?.toIntOrNull()?.takeIf { node.isHeading } ?: 0
    }

    /** The links inside the node's text, with the text each covers. */
    private fun spans(): List<Pair<String, ClickableSpan>> {
        val text = node.text as? Spanned ?: return emptyList()
        return text.getSpans(0, text.length, ClickableSpan::class.java)
            .sortedBy { text.getSpanStart(it) }
            .map { span -> text.subSequence(text.getSpanStart(span), text.getSpanEnd(span)).toString() to span }
    }

    override fun children(): List<NodeHandle> = (0 until node.childCount).mapNotNull { node.getChild(it)?.let { child -> AndroidNode(child, context) } }

    override fun perform(action: NodeAction, text: String): Boolean = perform(action, ActionArguments(text = text))

    override fun perform(action: NodeAction, arguments: ActionArguments): Boolean {
        val id = ACTIONS.firstOrNull { it.first == action }?.second ?: return false
        val bundle = when (action) {
            NodeAction.NODE_ACTION_SET_TEXT -> Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, arguments.text)
            }
            NodeAction.NODE_ACTION_SET_SELECTION -> Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_START_INT, arguments.selectionStart)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_SELECTION_END_INT, arguments.selectionEnd)
            }
            NodeAction.NODE_ACTION_SET_PROGRESS -> Bundle().apply {
                putFloat(AccessibilityNodeInfo.ACTION_ARGUMENT_PROGRESS_VALUE, arguments.progress)
            }
            NodeAction.NODE_ACTION_SCROLL_TO_POSITION -> Bundle().apply {
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_ROW_INT, arguments.row)
                putInt(AccessibilityNodeInfo.ACTION_ARGUMENT_COLUMN_INT, arguments.column)
            }
            else -> null
        }
        // Read again, so the action goes to the node as it is now.
        node.refresh()
        return node.performAction(id, bundle)
    }

    override fun performCustom(id: Int): Boolean {
        node.refresh()
        return node.performAction(id)
    }

    /** A link of a node's text is followed as the app's own click on it would. */
    override fun followLink(index: Int): Boolean {
        val span = spans().getOrNull(index)?.second ?: return false
        span.onClick(View(context))
        return true
    }

    private companion object {
        /** Where a web view puts a node's role in words. */
        const val ROLE_DESCRIPTION = "AccessibilityNodeInfo.roleDescription"

        /** The top byte of an app's own resource IDs, which its custom actions use. */
        const val APP_RESOURCES = 0x7f

        val ACTIONS = listOf(
            NodeAction.NODE_ACTION_CLICK to AccessibilityNodeInfo.ACTION_CLICK,
            NodeAction.NODE_ACTION_LONG_CLICK to AccessibilityNodeInfo.ACTION_LONG_CLICK,
            NodeAction.NODE_ACTION_SET_TEXT to AccessibilityNodeInfo.ACTION_SET_TEXT,
            NodeAction.NODE_ACTION_SCROLL_FORWARD to AccessibilityNodeInfo.ACTION_SCROLL_FORWARD,
            NodeAction.NODE_ACTION_SCROLL_BACKWARD to AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD,
            NodeAction.NODE_ACTION_FOCUS to AccessibilityNodeInfo.ACTION_FOCUS,
            NodeAction.NODE_ACTION_ACCESSIBILITY_FOCUS to AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS,
            NodeAction.NODE_ACTION_SELECT to AccessibilityNodeInfo.ACTION_SELECT,
            NodeAction.NODE_ACTION_SET_SELECTION to AccessibilityNodeInfo.ACTION_SET_SELECTION,
            NodeAction.NODE_ACTION_EXPAND to AccessibilityNodeInfo.ACTION_EXPAND,
            NodeAction.NODE_ACTION_COLLAPSE to AccessibilityNodeInfo.ACTION_COLLAPSE,
            NodeAction.NODE_ACTION_SET_PROGRESS to AccessibilityAction.ACTION_SET_PROGRESS.id,
            NodeAction.NODE_ACTION_SHOW_ON_SCREEN to AccessibilityAction.ACTION_SHOW_ON_SCREEN.id,
            NodeAction.NODE_ACTION_SCROLL_TO_POSITION to AccessibilityAction.ACTION_SCROLL_TO_POSITION.id,
        )
    }
}
