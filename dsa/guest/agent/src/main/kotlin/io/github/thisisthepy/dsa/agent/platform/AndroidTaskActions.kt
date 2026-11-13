package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityOptions
import android.app.ActivityTaskManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Insets
import android.graphics.Rect
import android.net.Uri
import android.os.Binder
import android.provider.Settings
import android.view.Display
import android.view.WindowInsets
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.agent.requests.SettingsPage
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.InsetsSourceForm
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM
import java.lang.reflect.InvocationTargetException

/** Starts, ends, moves and resizes tasks through the framework. */
class AndroidTaskActions(
    private val context: Context,
    private val log: Log,
) : TaskActions {
    private val themes = FullScreenThemes(context, log)
    private var warnedNoDensity = false
    private var warnedNoInsets = false

    /** Owns the inset sources the agent gives tasks, which name their owner. */
    private val insetsOwner = Binder()
    private var warnedNoScreenOverrides = false

    /** Whether this framework lacks the calls that hold a task's screen size. */
    private var noScreenOverrides = false

    /** The Settings pages this Settings lacks, each said once in the log. */
    private val missingPages = mutableSetOf<String>()

    override fun launchApp(packageName: String): Boolean = launch(packageName, Intent.FLAG_ACTIVITY_NEW_TASK)

    override fun supportsMultipleWindows(packageName: String): Boolean =
        io.github.thisisthepy.dsa.agent.platform.supportsMultipleWindows(context.packageManager, packageName)

    /**
     * Starts the launcher activity with FLAG_ACTIVITY_MULTIPLE_TASK beside NEW_TASK, which
     * makes the framework start another task rather than bring the app's task to the front,
     * as Android's desktop windows do for New window.
     */
    override fun launchNewWindow(packageName: String): Boolean =
        launch(packageName, Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK)

    /**
     * Starts the shortcut through LauncherApps, as the default launcher, in a freeform window
     * on the guest display like any app the host launches. Android throws when the app has no
     * such shortcut or it is turned off.
     */
    override fun launchShortcut(packageName: String, shortcutId: String): Boolean {
        val launcher = context.getSystemService(android.content.pm.LauncherApps::class.java) ?: return false
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        freeform(options)
        return try {
            launcher.startShortcut(packageName, shortcutId, null, options.toBundle(), android.os.Process.myUserHandle())
            true
        } catch (e: ActivityNotFoundException) {
            log.warn("$packageName has no shortcut $shortcutId to start, or it is turned off", e)
            false
        } catch (e: IllegalStateException) {
            log.warn("the shortcut $shortcutId of $packageName cannot start before the user's storage is unlocked", e)
            false
        } catch (e: SecurityException) {
            log.warn("the framework refused to start the shortcut $shortcutId of $packageName", e)
            false
        }
    }

    private fun launch(packageName: String, flags: Int): Boolean {
        val intent = context.packageManager.getLaunchIntentForPackage(packageName) ?: return false
        intent.addFlags(flags)
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        freeform(options)
        themes.startBounds(intent)?.let { options.setLaunchBounds(it) }
        return try {
            context.startActivity(intent, options.toBundle())
            true
        } catch (e: ActivityNotFoundException) {
            log.warn("$packageName has no activity to start", e)
            false
        } catch (e: SecurityException) {
            log.warn("the framework refused to start $packageName", e)
            false
        }
    }

    override fun openAppSettings(packageName: String?, page: SettingsPage): Boolean {
        if (packageName == null) return startSettings(Intent(Settings.ACTION_MANAGE_APPLICATIONS_SETTINGS), "the list of apps")
        val action = when (page) {
            SettingsPage.DETAILS -> Settings.ACTION_APPLICATION_DETAILS_SETTINGS
            // Settings' UserAspectRatioAppActivity, which takes the package as its data.
            SettingsPage.ASPECT_RATIO -> ACTION_MANAGE_USER_ASPECT_RATIO_SETTINGS
            SettingsPage.OPEN_BY_DEFAULT -> Settings.ACTION_APP_OPEN_BY_DEFAULT_SETTINGS
        }
        val data = Uri.fromParts("package", packageName, null)
        if (action == Settings.ACTION_APPLICATION_DETAILS_SETTINGS) return startSettings(Intent(action, data), packageName)
        return try {
            startSettings(Intent(action, data), packageName, rethrowMissing = true)
        } catch (e: ActivityNotFoundException) {
            // A Settings without this page still has the app's info, where the user finds it.
            if (missingPages.add(action)) log.warn("Settings has no $action page; the app's info opens instead", e)
            startSettings(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, data), packageName)
        }
    }

    /**
     * Starts the Settings page [intent] as a task of its own in a freeform window; [what] names
     * it in the log. A missing page is thrown on when [rethrowMissing], else it is false.
     */
    private fun startSettings(intent: Intent, what: String, rethrowMissing: Boolean = false): Boolean {
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK)
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        freeform(options)
        return try {
            context.startActivity(intent, options.toBundle())
            true
        } catch (e: ActivityNotFoundException) {
            if (rethrowMissing) throw e
            log.warn("Settings has no page for $what", e)
            false
        } catch (e: SecurityException) {
            log.warn("the framework refused to open the settings of $what", e)
            false
        }
    }

    override fun closeTask(taskId: Int): Boolean = ActivityTaskManager.getService().removeTask(taskId)

    /**
     * Focuses the task through IActivityTaskManager.setFocusedTask, which moves it to the
     * front of its display and makes its top window the one keys go to. A task that has
     * ended, or a framework without the call, refuses.
     */
    override fun focusTask(taskId: Int): Boolean = try {
        ActivityTaskManager.getService().setFocusedTask(taskId)
        true
    } catch (e: LinkageError) {
        log.warn("this framework has no setFocusedTask; keys go to whichever task the guest focused", e)
        false
    } catch (e: RuntimeException) {
        log.warn("the framework refused to focus task $taskId", e)
        false
    }

    /**
     * Moves a freeform task through IActivityTaskManager.resizeTask, the call behind
     * `cmd activity task resize`. It returns nothing: the window manager drops a request for a
     * task it cannot resize, and with shell transitions it may apply the bounds after the
     * call returns. The agent sees the result in the task's bounds.
     */
    override fun moveTask(taskId: Int, bounds: TaskBounds): Boolean = try {
        ActivityTaskManager.getService().resizeTask(
            taskId,
            Rect(bounds.left, bounds.top, bounds.right, bounds.bottom),
            RESIZE_MODE_PRESERVE_WINDOW,
        )
        true
    } catch (e: LinkageError) {
        log.warn("this framework has no resizeTask; tasks cannot be moved apart", e)
        false
    }

    /**
     * Gives the task [bounds], and its app [densityDpi] when one is asked for. A density goes
     * with the bounds in one window container transaction, so the app relays out once; if
     * that is not possible, the task still gets its bounds through [moveTask].
     */
    override fun resizeTask(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        if (densityDpi != null && applyWithDensity(taskId, bounds, densityDpi)) return ResizeOutcome.APPLIED
        return when {
            !moveTask(taskId, bounds) -> ResizeOutcome.REFUSED
            densityDpi == null -> ResizeOutcome.APPLIED
            else -> ResizeOutcome.BOUNDS_ONLY
        }
    }

    /**
     * Gives the task [bounds] while its configuration keeps [screen], in one window container
     * transaction (setBounds, setScreenSizeDp and setSmallestScreenWidthDp). The screen size in
     * dp is what an app's resources and layout are chosen by; held at [screen], only the
     * task's window configuration changes, which does not relaunch the task's activities, so
     * the app lays itself out again in the new bounds within a frame. Split screen holds a
     * task's configuration the same way while its divider is dragged. The orientation follows
     * from the screen size in dp, so it holds too. If this framework lacks the calls, the agent
     * logs it once and the task gets plain bounds through [moveTask].
     */
    override fun resizeTaskLive(taskId: Int, bounds: TaskBounds, screen: ScreenDp): ResizeOutcome {
        val pin = listOf(
            "setScreenSizeDp" to listOf(screen.widthDp, screen.heightDp),
            "setSmallestScreenWidthDp" to listOf(screen.smallestWidthDp),
        )
        if (!noScreenOverrides && applyTransaction(taskId, bounds, pin, ::warnNoScreenOverrides)) return ResizeOutcome.APPLIED
        return if (moveTask(taskId, bounds)) ResizeOutcome.BOUNDS_ONLY else ResizeOutcome.REFUSED
    }

    /**
     * Gives the task [bounds] and clears the screen size [resizeTaskLive] held, so the task's
     * configuration follows its bounds again, with [densityDpi] when one is asked for, all in
     * one window container transaction: the app is reconfigured once, at its final size and
     * density. A screen size of 0 (Configuration's SCREEN_WIDTH_DP_UNDEFINED and the others)
     * is how a transaction removes the override. If this framework lacks the calls, the task
     * was never held either, and this is [resizeTask].
     */
    override fun endLiveResize(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        val clear = mutableListOf(
            "setScreenSizeDp" to listOf(Configuration.SCREEN_WIDTH_DP_UNDEFINED, Configuration.SCREEN_HEIGHT_DP_UNDEFINED),
            "setSmallestScreenWidthDp" to listOf(Configuration.SMALLEST_SCREEN_WIDTH_DP_UNDEFINED),
        )
        if (densityDpi != null) clear += "setDensityDpi" to listOf(densityDpi)
        if (!noScreenOverrides && applyTransaction(taskId, bounds, clear, ::warnNoScreenOverrides)) return ResizeOutcome.APPLIED
        return resizeTask(taskId, bounds, densityDpi)
    }

    /**
     * Sets the task's bounds and density in one WindowContainerTransaction, applied through
     * the window organizer controller. This is how the framework's own desktop windowing
     * gives a task a density of its own (WindowContainerTransaction.setDensityDpi): the task's
     * configuration carries the density, so each task on the one display can draw at the
     * density of its own host monitor. False when this framework lacks the calls or refused.
     */
    private fun applyWithDensity(taskId: Int, bounds: TaskBounds, densityDpi: Int): Boolean =
        applyTransaction(taskId, bounds, listOf("setDensityDpi" to listOf(densityDpi)), ::warnNoDensity)

    /**
     * Applies one WindowContainerTransaction to the task through the window organizer
     * controller: setBounds with [bounds], then each of [calls], a method that takes the
     * task's token and the given ints. Applying a transaction needs MANAGE_ACTIVITY_TASKS, not
     * a registered organizer. None of these classes is in the SDK, so they are reached by
     * reflection. False when this framework lacks them or refused; [warn] hears why.
     */
    private fun applyTransaction(
        taskId: Int,
        bounds: TaskBounds,
        calls: List<Pair<String, List<Int>>>,
        warn: (Throwable) -> Unit,
    ): Boolean {
        return try {
            val token = taskToken(taskId) ?: return false
            val tokenClass = Class.forName("android.window.WindowContainerToken")
            val transactionClass = Class.forName("android.window.WindowContainerTransaction")
            val transaction = transactionClass.getConstructor().newInstance()
            transactionClass.getMethod("setBounds", tokenClass, Rect::class.java)
                .invoke(transaction, token, Rect(bounds.left, bounds.top, bounds.right, bounds.bottom))
            for ((name, args) in calls) {
                val types = arrayOf<Class<*>>(tokenClass) + args.map { Int::class.javaPrimitiveType!! }
                transactionClass.getMethod(name, *types).invoke(transaction, token, *args.toTypedArray())
            }
            commit(transactionClass, transaction)
        } catch (e: ReflectiveOperationException) {
            warn(e)
            false
        } catch (e: LinkageError) {
            warn(e)
            false
        } catch (e: RuntimeException) {
            warn(e)
            false
        }
    }

    /**
     * Applies [transaction] through the window organizer controller, which needs
     * MANAGE_ACTIVITY_TASKS, not a registered organizer. False when the framework has no
     * controller.
     */
    private fun commit(transactionClass: Class<*>, transaction: Any): Boolean {
        val controller = organizer
            ?: Class.forName("android.app.IActivityTaskManager")
                .getMethod("getWindowOrganizerController")
                .invoke(ActivityTaskManager.getService())
                ?.also { organizer = it }
            ?: return false
        Class.forName("android.window.IWindowOrganizerController")
            .getMethod("applyTransaction", transactionClass)
            .invoke(controller, transaction)
        return true
    }

    /**
     * Gives the task inset sources of the status bar and the caption bar types covering
     * [frame], or removes them when [frame] is null, in one window container transaction
     * (WindowContainerTransaction.addInsetsSource and removeInsetsSource). This is how the
     * framework's own desktop windowing gives a freeform task the inset of the caption it draws
     * over the task: the frame is in display pixels, and the task's windows receive the part
     * that overlaps them. The status bar type is given too because the host window's title bar
     * is Android's status bar: the guest has no status bar of its own, and most apps lay out
     * their top edge by the status bar inset, as on a phone. Stock Android hands a freeform
     * window only its caption bar and IME insets; the image's window manager keeps the status
     * bar inset too (make-image.py's SERVICES_CODE), so an app that pads by it keeps its
     * toolbar below the title bar. The sources do not force their insets to be consumed, so an
     * app drawing edge to edge draws its status bar background under the band, and Android
     * draws the status bar colour of an app that does not there.
     *
     * The caption bar source carries [boundingRects], relative to [frame], as Android's own
     * desktop windows give an app that draws its own title bar the places their caption draws
     * over (WindowDecoration's calculateBoundingRectsInsets); the app reads them with
     * WindowInsets.getBoundingRects(captionBar()). The status bar source carries none.
     */
    override fun setCaptionInset(taskId: Int, frame: TaskBounds?, boundingRects: List<TaskBounds>): Boolean = try {
        val token = taskToken(taskId)
        if (token == null) {
            false
        } else {
            val tokenClass = Class.forName("android.window.WindowContainerToken")
            val transactionClass = Class.forName("android.window.WindowContainerTransaction")
            val transaction = transactionClass.getConstructor().newInstance()
            val types = listOf(WindowInsets.Type.statusBars(), WindowInsets.Type.captionBar())
            for ((index, type) in types.withIndex()) {
                if (frame == null) {
                    transactionClass.getMethod(
                        "removeInsetsSource",
                        tokenClass,
                        android.os.IBinder::class.java,
                        Int::class.javaPrimitiveType,
                        Int::class.javaPrimitiveType,
                    ).invoke(transaction, token, insetsOwner, index, type)
                } else {
                    val rects = boundingRects
                        .takeIf { it.isNotEmpty() && type == WindowInsets.Type.captionBar() }
                        ?.map { Rect(it.left, it.top, it.right, it.bottom) }
                        ?.toTypedArray()
                    addInsetsSource(transactionClass, transaction, token, index, type, frame, rects)
                }
            }
            commit(transactionClass, transaction)
        }
    } catch (e: ReflectiveOperationException) {
        warnNoInsets(e)
        false
    } catch (e: LinkageError) {
        warnNoInsets(e)
        false
    } catch (e: RuntimeException) {
        warnNoInsets(e)
        false
    }

    /**
     * WindowContainerTransaction.addInsetsSource in whichever form this framework has: with
     * bounding rectangles and flags (Android 15 and later), with bounding rectangles, or with
     * the frame alone. [boundingRects] go where the form takes them; no flags are given.
     */
    private fun addInsetsSource(
        transactionClass: Class<*>,
        transaction: Any,
        token: Any,
        index: Int,
        type: Int,
        frame: TaskBounds,
        boundingRects: Array<Rect>?,
    ) {
        val methods = transactionClass.methods.filter { it.name == "addInsetsSource" }
        val form = InsetsSourceForm.choose(methods.map { m -> m.parameterTypes.map { it.typeName } })
            ?: throw NoSuchMethodException("WindowContainerTransaction.addInsetsSource")
        // A frame in display pixels, or the same band as a size from the task's top edge.
        val area: Any = when (form) {
            is InsetsSourceForm.Frame -> Rect(frame.left, frame.top, frame.right, frame.bottom)
            is InsetsSourceForm.Size -> Insets.of(0, frame.bottom - frame.top, 0, 0)
        }
        val count = when (form) {
            is InsetsSourceForm.Frame -> form.parameterCount
            is InsetsSourceForm.Size -> form.parameterCount
        }
        val args: Array<Any?> = when (count) {
            7 -> arrayOf(token, insetsOwner, index, type, area, boundingRects, 0)
            6 -> arrayOf(token, insetsOwner, index, type, area, boundingRects)
            else -> arrayOf(token, insetsOwner, index, type, area)
        }
        methods[form.index].invoke(transaction, *args)
    }

    /** The status bar heights read so far, by density. */
    private val statusBarHeights = HashMap<Int, Int>()

    /**
     * Reads the framework's status_bar_height at [densityDpi], as an app drawing at that
     * density reads it: the system configuration with only its density changed. The image's
     * framework overlay sets it to the title bar's height in pixels at each density Android
     * draws at on a monitor scale.
     */
    override fun statusBarHeight(densityDpi: Int): Int {
        if (densityDpi <= 0) return 0
        statusBarHeights[densityDpi]?.let { return it }
        val height = try {
            val system = android.content.res.Resources.getSystem()
            val id = system.getIdentifier("status_bar_height", "dimen", "android")
            if (id == 0) {
                0
            } else {
                val config = Configuration(system.configuration).apply { this.densityDpi = densityDpi }
                context.createConfigurationContext(config).resources.getDimensionPixelSize(id)
            }
        } catch (e: RuntimeException) {
            log.warn("could not read the status bar height at $densityDpi dpi", e)
            0
        }
        statusBarHeights[densityDpi] = height
        return height
    }

    private fun warnNoInsets(error: Throwable) {
        if (warnedNoInsets) return
        warnedNoInsets = true
        log.warn("could not give a task an inset through a window container transaction", error)
    }

    /**
     * The task's window container token (TaskInfo.token, not in the SDK). A task keeps its
     * token for as long as it lives, and a task ID is not given to another task while the
     * guest runs, so the tokens are kept: a resize then asks the framework for nothing before
     * its transaction. Reading the running tasks is a binder call that waits for the window
     * manager's lock and builds every task's description; on a busy guest it is one more wait
     * between a new size and the window manager, on the thread the size waits on. The tasks are
     * read again only for a task not seen yet, and the tokens of tasks that ended go then.
     */
    private fun taskToken(taskId: Int): Any? {
        tokens[taskId]?.let { return it }
        @Suppress("DEPRECATION")
        val running = context.getSystemService(ActivityManager::class.java).getRunningTasks(MAX_TASKS)
        val field = Class.forName("android.app.TaskInfo").getField("token")
        val read = HashMap<Int, Any>()
        for (info in running) field.get(info)?.let { read[info.taskId] = it }
        tokens = read
        return read[taskId]
    }

    /** Each running task's window container token, as [taskToken] last read them. */
    @Volatile
    private var tokens: Map<Int, Any> = emptyMap()

    /**
     * The window organizer controller, read once: it is the window manager's, which lives as
     * long as the guest does.
     */
    @Volatile
    private var organizer: Any? = null

    private fun warnNoDensity(error: Throwable) {
        if (warnedNoDensity) return
        warnedNoDensity = true
        log.warn("could not set a task's density through a window container transaction", error)
    }

    /**
     * Logs once that a task's screen size cannot be held. A framework that lacks the calls
     * (a missing class or method, not a refusal of one call) is not asked again, so each
     * step of a drag does not pay for the failed lookup.
     */
    private fun warnNoScreenOverrides(error: Throwable) {
        val missing = error is LinkageError || (error is ReflectiveOperationException && error !is InvocationTargetException)
        if (missing) noScreenOverrides = true
        if (warnedNoScreenOverrides) return
        warnedNoScreenOverrides = true
        log.warn(
            "could not hold a task's screen size through a window container transaction; " +
                "apps are reconfigured at each step of a resize",
            error,
        )
    }

    /**
     * Asks for a freeform window. On a freeform display that is what a new task gets anyway;
     * this covers a display the window manager has not switched yet. ActivityOptions'
     * setLaunchWindowingMode is not in the SDK, so it is called by reflection.
     */
    private fun freeform(options: ActivityOptions) {
        try {
            ActivityOptions::class.java.getMethod("setLaunchWindowingMode", Int::class.javaPrimitiveType)
                .invoke(options, WINDOWING_MODE_FREEFORM)
        } catch (e: ReflectiveOperationException) {
            log.warn("could not ask for a freeform window; the display's mode decides", e)
        }
    }

    private companion object {
        // ActivityTaskManager.RESIZE_MODE_PRESERVE_WINDOW: keep the app's window, do not
        // relaunch the activity.
        const val RESIZE_MODE_PRESERVE_WINDOW = 1

        const val MAX_TASKS = 1000

        // Settings.ACTION_MANAGE_USER_ASPECT_RATIO_SETTINGS, which the SDK hides. Settings of
        // Android 16 and 17 answers it with the package as data (UserAspectRatioAppActivity).
        const val ACTION_MANAGE_USER_ASPECT_RATIO_SETTINGS = "android.settings.MANAGE_USER_ASPECT_RATIO_SETTINGS"
    }
}
