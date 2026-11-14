package io.github.thisisthepy.dsa.agent.platform

import android.graphics.Rect
import android.os.Binder
import android.os.IBinder
import android.os.Parcel
import android.os.Parcelable
import android.os.ServiceManager
import android.view.Display
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.AppearanceArea
import io.github.thisisthepy.dsa.agent.tasks.BarsReport
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds

/**
 * The live appearance of the system bars, as the window manager reports it to the status bar.
 *
 * On a phone SystemUI registers its IStatusBar with the status bar service, and the window
 * manager's display policy calls its onSystemBarAttributesChanged whenever the window that
 * controls the bars, or the appearance that window asks for, changes; that is how the status
 * bar icons follow an app from screen to screen. The guest has no SystemUI, because the host is
 * the shell, so the agent registers in its place (IStatusBarService.registerStatusBar, which
 * takes STATUS_BAR_SERVICE, a permission of the platform's signature the agent holds).
 *
 * IStatusBar is a oneway interface of many methods that change between Android versions, so
 * the agent does not implement the generated stub: its binder answers every call, reads only
 * onSystemBarAttributesChanged (found by the stub's own transaction number, so it follows the
 * framework it runs on) and ignores the rest, as a status bar that shows nothing would. Only
 * the leading arguments are read (the display, the appearance flags, the appearance regions
 * and the package), which every Android since 12 sends first and in that order. The status
 * bar service holds one status bar; with no SystemUI in the guest, nothing else asks to be it.
 *
 * None of these classes are in the SDK; they are reached by reflection, which a platform app
 * may do. When any of it is missing, the agent says so once and reports no appearance, so the
 * host reads the tone from the app's pixels.
 */
class AndroidSystemBars(private val log: Log) {
    // Held so the status bar service's binder stays alive in this process.
    private var callback: Binder? = null

    /** Calls [onReport] with every report of the default display; false when it cannot. */
    fun listen(onReport: (BarsReport) -> Unit): Boolean {
        return try {
            val stub = Class.forName("$STATUS_BAR\$Stub")
            val code = transaction(stub, BARS_CHANGED)
            val regions = AppearanceRegions(Class.forName(APPEARANCE_REGION))
            val bar = StatusBarBinder(code, regions, log, onReport)
            val service = ServiceManager.getService(STATUS_BAR_SERVICE)
                ?: throw IllegalStateException("the guest has no status bar service")
            val barService = Class.forName("$STATUS_BAR_SERVICE_INTERFACE\$Stub")
                .getMethod("asInterface", IBinder::class.java)
                .invoke(null, service)
            val asBar = stub.getMethod("asInterface", IBinder::class.java).invoke(null, bar)
            val result = Class.forName(STATUS_BAR_SERVICE_INTERFACE)
                .getMethod("registerStatusBar", Class.forName(STATUS_BAR))
                .invoke(barService, asBar)
            callback = bar
            initial(result, regions)?.let(onReport)
            true
        } catch (e: ReflectiveOperationException) {
            log.warn("could not listen to the system bars' appearance; caption tones follow the apps' pixels", e)
            false
        } catch (e: LinkageError) {
            log.warn("this framework cannot report the system bars' appearance; caption tones follow the apps' pixels", e)
            false
        } catch (e: RuntimeException) {
            log.warn("the status bar service refused the agent; caption tones follow the apps' pixels", e)
            false
        }
    }

    /**
     * The transaction number of [method] in the framework's IStatusBar: the stub's
     * TRANSACTION_ constant, else the number its transaction names give that method.
     */
    private fun transaction(stub: Class<*>, method: String): Int {
        try {
            return stub.getDeclaredField("TRANSACTION_$method").apply { isAccessible = true }.getInt(null)
        } catch (e: NoSuchFieldException) {
            val name = stub.getMethod("getDefaultTransactionName", Int::class.javaPrimitiveType)
            for (code in IBinder.FIRST_CALL_TRANSACTION..IBinder.FIRST_CALL_TRANSACTION + MAX_METHODS) {
                if (name.invoke(null, code) == method) return code
            }
            throw e
        }
    }

    /**
     * What the status bar service says the bars look like when the agent registers
     * (RegisterStatusBarResult's appearance, regions and package), so an agent that starts
     * after the apps does not wait for their next change. Null when it does not say.
     */
    private fun initial(result: Any?, regions: AppearanceRegions): BarsReport? {
        if (result == null) return null
        return try {
            val type = result.javaClass
            val appearance = type.getField("mAppearance").getInt(result)
            @Suppress("UNCHECKED_CAST")
            val areas = (type.getField("mAppearanceRegions").get(result) as Array<Any?>?)
                ?.mapNotNull(regions::area)
                .orEmpty()
            val packageName = try {
                type.getField("mPackageName").get(result) as String?
            } catch (e: ReflectiveOperationException) {
                null
            }
            BarsReport(appearance, areas, packageName)
        } catch (e: ReflectiveOperationException) {
            null
        } catch (e: RuntimeException) {
            null
        }
    }

    /** com.android.internal.view.AppearanceRegion: its parcel creator and its two fields. */
    private class AppearanceRegions(type: Class<*>) {
        @Suppress("UNCHECKED_CAST")
        val creator = type.getField("CREATOR").get(null) as Parcelable.Creator<Any?>
        private val getAppearance = type.getMethod("getAppearance")
        private val getBounds = type.getMethod("getBounds")

        fun area(region: Any?): AppearanceArea? {
            if (region == null) return null
            val bounds = getBounds.invoke(region) as Rect? ?: return null
            return AppearanceArea(
                getAppearance.invoke(region) as Int,
                TaskBounds(bounds.left, bounds.top, bounds.right, bounds.bottom),
            )
        }
    }

    /** The status bar's binder: it reads one call, and takes every other one as done. */
    private class StatusBarBinder(
        private val barsCode: Int,
        private val regions: AppearanceRegions,
        private val log: Log,
        private val onReport: (BarsReport) -> Unit,
    ) : Binder() {
        private var warned = false

        init {
            attachInterface(null, STATUS_BAR)
        }

        override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
            if (code < IBinder.FIRST_CALL_TRANSACTION || code > IBinder.LAST_CALL_TRANSACTION) {
                return super.onTransact(code, data, reply, flags)
            }
            if (code != barsCode) return true
            try {
                data.enforceInterface(STATUS_BAR)
                val displayId = data.readInt()
                val appearance = data.readInt()
                val areas = data.createTypedArray(regions.creator)?.mapNotNull(regions::area).orEmpty()
                // navbarColorManagedByIme, behavior, requestedVisibleTypes, then the package.
                val packageName = try {
                    data.readInt()
                    data.readInt()
                    data.readInt()
                    data.readString()
                } catch (e: RuntimeException) {
                    null
                }
                if (displayId == Display.DEFAULT_DISPLAY) onReport(BarsReport(appearance, areas, packageName))
            } catch (e: Exception) {
                if (!warned) {
                    warned = true
                    log.warn("could not read a report of the system bars' appearance", e)
                }
            }
            return true
        }
    }

    private companion object {
        const val STATUS_BAR = "com.android.internal.statusbar.IStatusBar"
        const val STATUS_BAR_SERVICE_INTERFACE = "com.android.internal.statusbar.IStatusBarService"
        const val APPEARANCE_REGION = "com.android.internal.view.AppearanceRegion"
        const val BARS_CHANGED = "onSystemBarAttributesChanged"

        // More than IStatusBar has methods, for the search by transaction name.
        const val MAX_METHODS = 400

        // Context.STATUS_BAR_SERVICE.
        const val STATUS_BAR_SERVICE = "statusbar"
    }
}
