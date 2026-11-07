package io.github.thisisthepy.dsa.agent.platform

import java.io.File
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * A View's constructor calls overridable methods such as requestLayout() and invalidate()
 * before a Kotlin subclass has set its own properties. A callback kept as a constructor
 * property (`private val done: () -> Unit`) is therefore still null when an override reads
 * it, and the view throws while it is being made. That is how placed widgets stopped
 * drawing: every widget's window root threw in FrameLayout's constructor.
 *
 * The agent's tests run on the JVM without a View implementation, so this reads the source:
 * no View subclass in the agent may take a function as a constructor property.
 */
class ViewCallbackTest {
    private val viewBases = listOf(
        "FrameLayout", "LinearLayout", "ViewGroup", "View", "AppWidgetHostView", "SurfaceView", "TextureView",
    )

    // One line: `class Name(parameters) : Base(`; the parameters may hold `() -> Unit`.
    private val header = Regex("""class\s+(\w+)\s*\((.*)\)\s*:\s*(\w+)\s*\(""")

    private val functionProperty = Regex("""\bva[lr]\s+\w+\s*:\s*\([^,]*->""")

    @Test
    fun fr12_no_view_subclass_reads_a_constructor_callback_before_it_is_set() {
        val offenders = File("src/main/kotlin").walkTopDown()
            .filter { it.isFile && it.extension == "kt" }
            .flatMap { file ->
                header.findAll(file.readText())
                    .filter { it.groupValues[3] in viewBases && functionProperty.containsMatchIn(it.groupValues[2]) }
                    .map { "${file.name}: ${it.groupValues[1]}" }
            }
            .toList()
        assertTrue(
            "these View subclasses keep a callback as a constructor property, which is null while the " +
                "View constructor runs their overrides; keep it in a nullable field read with ?.invoke(): $offenders",
            offenders.isEmpty(),
        )
    }
}
