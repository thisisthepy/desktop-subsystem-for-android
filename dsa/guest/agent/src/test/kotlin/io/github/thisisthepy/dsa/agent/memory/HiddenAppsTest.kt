package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The image's app memory scripts as the agent sees them: each request is recorded, and the
 * trim script answers with the request's number and [trimResult] as soon as it is set (the
 * number alone when [trimResult] is null, as an older script does), or never when [answers] is
 * off. [hiddenTrim] is whether the framework has the activity manager's dsa-hidden-trim,
 * [takesHidden] whether it takes the apps the host hides, and [hiddenSets] what it was told.
 * Installed packages and the records are kept in memory.
 */
class FakeAppMemory : AppMemoryPort {
    val trims = mutableListOf<String>()
    val compiles = mutableListOf<String>()
    var answers = true
    var trimResult: String? = "sent"
    var hiddenTrim = false
    var takesHidden = true
    val hiddenSets = mutableListOf<Set<String>>()
    var supportAsked = 0
    var compileResult: String? = "compiled"
    val packages = mutableMapOf<String, InstalledPackage>()
    val saved = mutableMapOf<String, CompileRecord>()
    private var trimAnswer = ""
    private var compileAnswer = ""

    /** The trim requests without their numbers. */
    fun trimmed(): List<String> = trims.map { it.substringAfter(' ') }

    /** The packages asked to be compiled. */
    fun compiled(): List<String> = compiles.map { it.substringAfter(' ') }

    override fun requestTrim(value: String) {
        trims += value
        if (answers) trimAnswer = value.substringBefore(' ') + (trimResult?.let { " $it" } ?: "")
    }

    override fun hiddenTrimSupported(): Boolean {
        supportAsked++
        return hiddenTrim
    }

    override fun setHiddenApps(packages: Set<String>): Boolean {
        hiddenSets += packages
        return takesHidden
    }

    override fun trimDone() = trimAnswer

    override fun requestCompile(value: String) {
        compiles += value
        compileResult?.let { compileAnswer = "${value.substringBefore(' ')} $it" }
    }

    override fun compileDone() = compileAnswer

    override fun installed(packageName: String) = packages[packageName]

    override fun records(): Map<String, CompileRecord> = saved.toMap()

    override fun save(packageName: String, record: CompileRecord) {
        saved[packageName] = record
    }

    override fun wallMillis() = 1_700_000_000_000L
}

class HiddenAppsTest {
    private val port = FakeAppMemory()
    private val scheduler = FakeScheduler()
    private val log = RecordingLog()
    private val playing = mutableSetOf<String>()
    private val foreground = mutableSetOf<String>()
    private val apps = HiddenApps(
        port,
        scheduler,
        log,
        clock = { scheduler.now },
        playing = { playing },
        hasForegroundService = { it in foreground },
        ownPackage = AGENT,
    )

    @Test
    fun nfr1_an_app_going_out_of_sight_is_logged_once_for_each_spell() {
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3))
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3))
        val line = "info: $MUSIC is out of the user's sight (the host says none of its windows can be seen); " +
            "it is trimmed after 10 s unless it plays audio or runs a foreground service"
        assertEquals(log.lines.toString(), 1, log.lines.count { it == line })
        apps.shown(setOf(3, 5))
        apps.shown(setOf(3))
        assertEquals(2, log.lines.count { it == line })
    }

    @Test
    fun nfr1_a_hidden_app_is_trimmed_then_compacted_once() {
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3, 5))
        scheduler.advance(10 * 60_000L)
        assertEquals(emptyList<String>(), port.trims)
        // Task 5's window is minimized or covered: its app is trimmed after ten seconds, then
        // sent the background level and compacted after two minutes, and nothing more.
        apps.shown(setOf(3))
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS - 1)
        assertEquals(emptyList<String>(), port.trims)
        scheduler.advance(1)
        assertEquals(listOf("HIDDEN 0 $MUSIC"), port.trimmed())
        scheduler.advance(HiddenApps.BACKGROUND_AFTER_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC", "BACKGROUND 1 $MUSIC"), port.trimmed())
        scheduler.advance(30 * 60_000L)
        assertEquals(2, port.trims.size)
        assertTrue(log.lines.any { "trimming $MUSIC, which the user cannot see" in it })
        // Seen again, nothing is undone; hidden again, it is trimmed again.
        apps.shown(setOf(3, 5))
        scheduler.advance(60_000L)
        apps.shown(setOf(3))
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        assertEquals(3, port.trims.size)
        // Seen again within two minutes, it is not sent the background level.
        apps.shown(setOf(3, 5))
        scheduler.advance(10 * 60_000L)
        assertEquals(3, port.trims.size)
    }

    @Test
    fun nfr1_an_app_playing_audio_or_with_a_foreground_service_is_not_trimmed() {
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC, 7 to MAPS))
        playing += MUSIC
        foreground += MAPS
        apps.screen(false)
        scheduler.advance(HiddenApps.BACKGROUND_AFTER_MILLIS)
        // With no window shown every app is hidden; only the quiet one is trimmed.
        assertEquals(listOf("HIDDEN 0 $NOTES", "BACKGROUND 1 $NOTES"), port.trimmed())
        assertEquals(1, log.lines.count { it == "info: left hidden $MUSIC untrimmed: it plays audio" })
        assertEquals(1, log.lines.count { it == "info: left hidden $MAPS untrimmed: it runs a foreground service" })
        // Once the music stops, it is trimmed at the next look.
        playing.clear()
        scheduler.advance(HiddenApps.RECHECK_MILLIS)
        assertTrue(port.trimmed().contains("BACKGROUND 1 $MUSIC"))
        assertTrue(port.trimmed().none { it.endsWith(MAPS) })
    }

    @Test
    fun nfr1_trims_go_one_at_a_time_and_the_agent_is_never_trimmed() {
        port.answers = false
        apps.tasks(mapOf(1 to AGENT, 3 to NOTES, 5 to MUSIC))
        apps.shown(emptySet())
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        // The second waits for the first's answer, which never comes, so for the time limit.
        assertEquals(listOf("HIDDEN 0 $MUSIC"), port.trimmed())
        scheduler.advance(HiddenApps.REQUEST_TIMEOUT_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC", "HIDDEN 0 $NOTES"), port.trimmed())
        assertTrue(port.trimmed().none { it.endsWith(AGENT) })
        // Each request has a number of its own, so init sees each as a change.
        assertEquals(port.trims.size, port.trims.map { it.substringBefore(' ') }.toSet().size)
    }

    @Test
    fun nfr1_until_the_host_says_which_windows_are_seen_every_task_counts_as_seen() {
        apps.tasks(mapOf(3 to NOTES))
        scheduler.advance(10 * 60_000L)
        assertEquals(emptyList<String>(), port.trims)
    }

    @Test
    fun nfr1_a_task_opened_after_the_hosts_last_word_counts_as_seen() {
        // NAVER reopened as task 148 on build 1.0.1-dev.12 was logged out of sight as its
        // window opened: the host's last list of seen windows predated the task.
        apps.tasks(mapOf(3 to NOTES))
        apps.shown(setOf(3))
        apps.tasks(mapOf(3 to NOTES, 148 to MAPS))
        assertTrue(log.lines.toString(), log.lines.none { it.startsWith("info: $MAPS is out of the user's sight") })
        // The host's next word counts.
        apps.shown(setOf(3))
        assertTrue(log.lines.toString(), log.lines.any { it.startsWith("info: $MAPS is out of the user's sight") })
    }

    @Test
    fun nfr1_a_hidden_app_receives_ui_hidden_after_ten_seconds() {
        // With DSA's framework the activity manager is told which apps the host hides, so the
        // trim reaches an app although Android shows its task.
        port.hiddenTrim = true
        apps.tasks(mapOf(1 to AGENT, 3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(1, 3, 5))
        assertEquals(listOf(emptySet<String>()), port.hiddenSets)
        apps.shown(setOf(1, 3))
        assertEquals(setOf(MUSIC), port.hiddenSets.last())
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC"), port.trimmed())
        scheduler.advance(HiddenApps.POLL_MILLIS)
        assertTrue(log.lines.toString(), log.lines.none { it.startsWith("warn:") })
        assertEquals(1, log.lines.count { "the framework sends the apps the host hides their memory trims" in it })
        // The same word again tells the framework nothing new; seen again, the app leaves the set.
        apps.shown(setOf(1, 3))
        assertEquals(2, port.hiddenSets.size)
        apps.shown(setOf(1, 3, 5))
        assertEquals(emptySet<String>(), port.hiddenSets.last())
        // The agent itself is never in it.
        apps.shown(emptySet())
        assertEquals(setOf(NOTES, MUSIC), port.hiddenSets.last())
        // The framework is asked once whether it has the change.
        assertEquals(1, port.supportAsked)
    }

    @Test
    fun nfr1_hidden_trim_reports_the_frameworks_refusal() {
        port.trimResult = "refused Unable to set a background trim level on a foreground process"
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3))
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        scheduler.advance(HiddenApps.POLL_MILLIS)
        val refused = "warn: the activity manager did not send $MUSIC TRIM_MEMORY_UI_HIDDEN: " +
            "Unable to set a background trim level on a foreground process"
        assertEquals(log.lines.toString(), 1, log.lines.count { it == refused })
        // A refusal is still an answer: the later request is sent, and its refusal logged too.
        scheduler.advance(HiddenApps.BACKGROUND_AFTER_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC", "BACKGROUND 1 $MUSIC"), port.trimmed())
        assertEquals(1, log.lines.count { it.startsWith("warn: the activity manager did not send $MUSIC TRIM_MEMORY_BACKGROUND: ") })
    }

    @Test
    fun nfr1_without_the_frameworks_hidden_trim_trims_go_as_before_and_it_is_logged_once() {
        port.hiddenTrim = false
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3))
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        apps.shown(setOf(3, 5))
        apps.shown(setOf(3))
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC", "HIDDEN 0 $MUSIC"), port.trimmed())
        assertEquals(emptyList<Set<String>>(), port.hiddenSets)
        assertEquals(1, port.supportAsked)
        assertEquals(1, log.lines.count { "this framework has no dsa-hidden-trim" in it })
    }

    @Test
    fun nfr1_a_framework_that_does_not_take_the_hidden_apps_is_told_again_and_it_is_logged_once() {
        port.hiddenTrim = true
        port.takesHidden = false
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(setOf(3))
        apps.shown(setOf(3))
        assertEquals(listOf(emptySet(), setOf(MUSIC), setOf(MUSIC)), port.hiddenSets)
        assertEquals(1, log.lines.count { it.startsWith("warn: the activity manager did not take the apps the host hides") })
        // The trim is still asked for: the framework answers for itself.
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC"), port.trimmed())
    }

    @Test
    fun nfr1_a_trim_script_that_answers_the_number_alone_still_lets_the_next_request_go() {
        port.trimResult = null
        apps.tasks(mapOf(3 to NOTES, 5 to MUSIC))
        apps.shown(emptySet())
        scheduler.advance(HiddenApps.HIDDEN_AFTER_MILLIS)
        scheduler.advance(HiddenApps.POLL_MILLIS)
        assertEquals(listOf("HIDDEN 0 $MUSIC", "HIDDEN 0 $NOTES"), port.trimmed())
        assertTrue(log.lines.toString(), log.lines.none { it.startsWith("warn:") })
    }

    private companion object {
        const val AGENT = "io.github.thisisthepy.dsa.agent"
        const val NOTES = "org.example.notes"
        const val MUSIC = "com.spotify.music"
        const val MAPS = "org.example.maps"
    }
}
