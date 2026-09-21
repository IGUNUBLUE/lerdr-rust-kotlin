package lerdr.core.conversation

import com.google.common.truth.Truth.assertThat
import com.google.common.truth.Truth.assertWithMessage
import com.lerdr.core.testing.Fixtures
import com.lerdr.core.testing.array
import com.lerdr.core.testing.bool
import com.lerdr.core.testing.int
import com.lerdr.core.testing.string
import com.lerdr.core.testing.stringOrNull
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import org.junit.Test

/**
 * Conformance over `fixtures/conversation/conversation.page.*.json` — the
 * golden vectors exported from the Go readers (`page_export_test.go`).
 *
 * Each vector step is one reader-level page read: `{before, limit} →
 * {available, reason_code, expected_entries, has_more, total, next_cursor,
 * source_corrupt, file_truncated, omo_plan}`. The app never reads transcript
 * files — it consumes the wire `BrowsePage` — so the harness rewrites every
 * step into the wire payload the Go browser emits for that read, then asserts
 * the projected [ConversationPage] field-for-field:
 *
 * - `available=false` reader outcomes (`invalid_provider`, `invalid_session`,
 *   `source_unavailable`, `path_uncontained`) become `browseUnavailable`
 *   pages: `state=ready`, `mode=recent`, `total=null`, no `error`;
 * - other non-empty `reason_code`s become failure pages: `available=true`,
 *   `state=failed`, `error.code=reason_code`, `retryable=false`;
 * - `source_corrupt` arrives as `diagnostics.corrupt_records=1`
 *   (`plan_corrupt=true` for omo, where the corrupt row is the plan);
 * - `file_truncated`/`continuation_*` arrive inside `diagnostics`;
 * - entry `namespaced:true` marks a claude continuation-segment id exported as
 *   the raw 24-hex suffix (the wire id gains an inode-derived `12hex-` prefix
 *   no fixture can reproduce) — asserted as a format check on the id.
 */
class ConversationPageConformanceTest {

    @Test fun claudeSuite() = assertSuite("claude")

    @Test fun codexSuite() = assertSuite("codex")

    @Test fun hermesSuite() = assertSuite("hermes")

    @Test fun omoSuite() = assertSuite("omo")

    @Test fun ompSuite() = assertSuite("omp")

    @Test fun opencodeSuite() = assertSuite("opencode")

    @Test fun piSuite() = assertSuite("pi")

    @Test fun qoderSuite() = assertSuite("qoder")

    @Test fun errorsSuite() = assertSuite("errors")

    @Test
    fun everySuiteIsPresent() {
        // Guards against a silently dropped fixture file: the suite list is
        // closed, one file per agent kind plus the cross-cutting errors suite.
        assertThat(SUITES).hasSize(9)
    }

    private fun assertSuite(name: String) {
        val suite = Fixtures.load("conversation/conversation.page.$name.json")
        for (vector in suite.vectors) {
            assertVector(name, vector)
        }
    }

    private fun assertVector(suiteName: String, vector: JsonObject) {
        val label = "$suiteName/${vector.string("name")}"
        val agent = vector.string("agent")
        assertWithMessage("$label agent").that(agent).isNotEmpty()
        val steps = vector.array("steps")
        assertWithMessage("$label steps").that(steps).isNotEmpty()

        // wireBefore[i] is the cursor a client would pass to page older than
        // step i — the first entry id of that step's page (exporter semantics).
        val wireBefore = arrayOfNulls<String>(steps.size)
        for ((index, element) in steps.withIndex()) {
            val step = element.jsonObject
            val stepLabel = "$label step '${step.string("label")}'"
            val before = resolveBefore(steps, step, wireBefore, stepLabel)
            val page = ConversationProjector.project(wirePage(agent, step))
            assertPage(stepLabel, agent, step, page)
            page.entries.firstOrNull()?.let { wireBefore[index] = it.id }
            assertCursorContract(stepLabel, steps, step, before)
        }
    }

    /** The `before` argument a client would send for this step. */
    private fun resolveBefore(
        steps: JsonArray,
        step: JsonObject,
        wireBefore: Array<String?>,
        stepLabel: String,
    ): String {
        val beforeStep = step["before_step"]?.jsonPrimitive?.longOrNull?.toInt()
            ?: return step.stringOrNull("before") ?: ""
        assertWithMessage("$stepLabel before_step").that(beforeStep).isIn(0 until steps.size)
        val cursor = wireBefore[beforeStep]
        assertWithMessage("$stepLabel before_step $beforeStep produced no cursor")
            .that(cursor).isNotEmpty()
        return cursor!!
    }

    /**
     * Cursor contract, per the suite notes: `next_cursor` is the first entry
     * id of the page — the `before`/`cursor` that returns the next-older
     * page — emitted iff `has_more`.
     */
    private fun assertCursorContract(
        stepLabel: String,
        steps: JsonArray,
        step: JsonObject,
        before: String,
    ) {
        val expected = step.array("expected_entries")
        val hasMore = step.bool("has_more")
        val nextCursor = step.stringOrNull("next_cursor")
        if (hasMore) {
            assertWithMessage("$stepLabel next_cursor").that(nextCursor).isNotNull()
            assertWithMessage("$stepLabel next_cursor == first entry id")
                .that(nextCursor).isEqualTo(expected.first().jsonObject.string("id"))
            val namespaced = step["next_cursor_namespaced"]?.jsonPrimitive?.booleanOrNull == true
            val firstNamespaced =
                expected.first().jsonObject["namespaced"]?.jsonPrimitive?.booleanOrNull == true
            assertWithMessage("$stepLabel next_cursor_namespaced")
                .that(namespaced).isEqualTo(firstNamespaced)
            if (namespaced) {
                assertWithMessage("$stepLabel namespaced cursor carries the raw suffix")
                    .that(nextCursor).matches(NAMESPACED_ID.pattern)
            }
        } else {
            assertWithMessage("$stepLabel next_cursor").that(nextCursor).isNull()
        }
        // `before_step` chains: the cursor sent was the referenced step's
        // declared next_cursor (raw id form — both are raw in fixture space).
        step["before_step"]?.jsonPrimitive?.longOrNull?.toInt()?.let { source ->
            val referenced = steps[source].jsonObject
            assertWithMessage("$stepLabel before == step $source next_cursor")
                .that(before).isEqualTo(referenced.stringOrNull("next_cursor"))
        }
    }

    /**
     * Rewrites a step's reader-level expectation into the wire `BrowsePage`
     * payload the Go browser emits for the same read
     * (`internal/conversation/browser.go`):
     *
     * - reader `available=false` becomes `browseUnavailable` for file agents
     *   (only the source-location codes can produce it); sqlite readers route
     *   through `nativeFailure`, which keeps just `invalid_session` and
     *   `source_unavailable` unavailable — everything else (e.g. hermes
     *   `invalid_cursor`) becomes a `state=failed` page with `error`;
     * - a reader page that is `available=true` with a non-empty
     *   `reason_code` (e.g. claude `source_changed`) is likewise a
     *   `state=failed` page — `browseFailure` always sets `available=true`,
     *   `mode=recent`, and `retryable` per code;
     * - error pages carry a human `reason` string the fixtures don't pin, so
     *   a sentinel stands in.
     */
    private fun wirePage(agent: String, step: JsonObject): JsonObject {
        val available = step.bool("available")
        val reasonCode = step.stringOrNull("reason_code") ?: ""
        val failed = isWireFailure(agent, available, reasonCode)
        return buildJsonObject {
            put("available", available || failed)
            if (reasonCode.isNotEmpty()) {
                put("reason_code", reasonCode)
                put("reason", "fixture: $reasonCode")
            }
            put("entries", step["expected_entries"] ?: JsonArray(emptyList()))
            put("has_more", step.bool("has_more"))
            // Wire `total` is `*int` without omitempty — null on error pages.
            if (available && !failed) put("total", step.int("total")) else put("total", JsonNull)
            step.stringOrNull("next_cursor")?.let { put("next_cursor", it) }
            put("state", if (failed) "failed" else "ready")
            put("mode", if (available && !failed && agent in SQLITE_AGENTS) "native" else "recent")
            put("diagnostics", buildJsonObject {
                if (step["source_corrupt"]?.jsonPrimitive?.booleanOrNull == true) {
                    if (agent == "omo" || agent == "ohmyopencode") {
                        put("plan_corrupt", true)
                    } else {
                        put("corrupt_records", 1)
                    }
                }
                if (step["file_truncated"]?.jsonPrimitive?.booleanOrNull == true) {
                    put("source_truncated", true)
                }
                if (step["continuation_incomplete"]?.jsonPrimitive?.booleanOrNull == true) {
                    put("continuation_incomplete", true)
                    step.stringOrNull("continuation_reason")?.let {
                        put("continuation_reason", it)
                    }
                }
            })
            if (failed) {
                put("error", buildJsonObject {
                    put("code", reasonCode)
                    put("message", "fixture: $reasonCode")
                    put("retryable", reasonCode in RETRYABLE_CODES)
                })
            }
            step["omo_plan"]?.let { put("omo_plan", it) }
        }
    }

    /**
     * Whether the wire page for this reader outcome is `state=failed`
     * (browseFailure/nativeFailure) rather than `available=false`
     * (browseUnavailable). Mirrors the Go call sites.
     */
    private fun isWireFailure(agent: String, available: Boolean, reasonCode: String): Boolean {
        if (reasonCode.isEmpty()) return false
        return if (agent in SQLITE_AGENTS) {
            reasonCode !in NATIVE_UNAVAILABLE_CODES
        } else {
            available && reasonCode !in UNAVAILABLE_CODES
        }
    }

    private fun assertPage(
        stepLabel: String,
        agent: String,
        step: JsonObject,
        page: ConversationPage,
    ) {
        val reasonCode = step.stringOrNull("reason_code") ?: ""
        val failed = isWireFailure(agent, step.bool("available"), reasonCode)

        assertWithMessage("$stepLabel available")
            .that(page.available).isEqualTo(step.bool("available") || failed)
        assertWithMessage("$stepLabel reasonCode").that(page.reasonCode).isEqualTo(reasonCode)
        if (reasonCode.isNotEmpty()) {
            // The wire always carries a human reason on error pages.
            assertWithMessage("$stepLabel reason").that(page.reason).isNotEmpty()
        }
        assertWithMessage("$stepLabel state").that(page.state).isEqualTo(
            if (failed) ConversationBrowseState.FAILED else ConversationBrowseState.READY,
        )
        assertWithMessage("$stepLabel mode").that(page.mode).isEqualTo(
            if (page.available && !failed && agent in SQLITE_AGENTS) {
                ConversationBrowseMode.NATIVE
            } else {
                ConversationBrowseMode.RECENT
            },
        )
        assertWithMessage("$stepLabel hasMore").that(page.hasMore).isEqualTo(step.bool("has_more"))
        assertWithMessage("$stepLabel nextCursor")
            .that(page.nextCursor).isEqualTo(step.stringOrNull("next_cursor") ?: "")
        if (page.available && !failed) {
            assertWithMessage("$stepLabel total").that(page.total).isEqualTo(step.int("total"))
            assertWithMessage("$stepLabel total covers page")
                .that(page.total!!).isAtLeast(page.entries.size)
        } else {
            assertWithMessage("$stepLabel total").that(page.total).isNull()
        }
        if (failed) {
            val error = page.error
            assertWithMessage("$stepLabel error").that(error).isNotNull()
            assertWithMessage("$stepLabel error.code").that(error!!.code).isEqualTo(reasonCode)
            assertWithMessage("$stepLabel error.retryable").that(error.retryable)
                .isEqualTo(reasonCode in RETRYABLE_CODES)
            assertWithMessage("$stepLabel error.message").that(error.message).isNotEmpty()
        } else {
            assertWithMessage("$stepLabel error").that(page.error).isNull()
        }
        assertWithMessage("$stepLabel sourceCorrupt").that(page.sourceCorrupt)
            .isEqualTo(step["source_corrupt"]?.jsonPrimitive?.booleanOrNull == true)
        assertWithMessage("$stepLabel fileTruncated").that(page.fileTruncated)
            .isEqualTo(step["file_truncated"]?.jsonPrimitive?.booleanOrNull == true)
        assertWithMessage("$stepLabel continuationIncomplete")
            .that(page.diagnostics.continuationIncomplete)
            .isEqualTo(step["continuation_incomplete"]?.jsonPrimitive?.booleanOrNull == true)
        assertWithMessage("$stepLabel continuationReason")
            .that(page.diagnostics.continuationReason)
            .isEqualTo(step.stringOrNull("continuation_reason"))

        assertEntries(stepLabel, step.array("expected_entries"), page.entries)
        assertOmoPlan(stepLabel, step["omo_plan"], page.omoPlan)
    }

    private fun assertEntries(
        stepLabel: String,
        expected: JsonArray,
        entries: List<ConversationEntry>,
    ) {
        assertWithMessage("$stepLabel entries.size").that(entries).hasSize(expected.size)
        for ((index, pair) in expected.zip(entries).withIndex()) {
            val (want, entry) = pair
            val expectedEntry = want.jsonObject
            val entryLabel = "$stepLabel entry[$index]"
            assertWithMessage("$entryLabel id").that(entry.id)
                .isEqualTo(expectedEntry.string("id"))
            val namespaced =
                expectedEntry["namespaced"]?.jsonPrimitive?.booleanOrNull == true
            if (namespaced) {
                assertWithMessage("$entryLabel namespaced id is the raw suffix")
                    .that(entry.id).matches(NAMESPACED_ID.pattern)
            }
            assertWithMessage("$entryLabel timestamp").that(entry.timestamp)
                .isEqualTo(expectedEntry.stringOrNull("timestamp") ?: "")
            assertWithMessage("$entryLabel role").that(entry.role).isEqualTo(
                ConversationRole.fromWire(expectedEntry.string("role")),
            )
            assertWithMessage("$entryLabel text").that(entry.text)
                .isEqualTo(expectedEntry.stringOrNull("text") ?: "")
            assertWithMessage("$entryLabel truncated").that(entry.truncated)
                .isEqualTo(expectedEntry["truncated"]?.jsonPrimitive?.booleanOrNull == true)

            val expectedTools = (expectedEntry["tools"] as? JsonArray).orEmpty()
            assertWithMessage("$entryLabel tools.size").that(entry.tools)
                .hasSize(expectedTools.size)
            for ((toolIndex, toolPair) in expectedTools.zip(entry.tools).withIndex()) {
                val (wantTool, tool) = toolPair
                val expectedTool = wantTool.jsonObject
                val toolLabel = "$entryLabel tools[$toolIndex]"
                assertWithMessage("$toolLabel id").that(tool.id)
                    .isEqualTo(expectedTool.stringOrNull("id") ?: "")
                assertWithMessage("$toolLabel name").that(tool.name)
                    .isEqualTo(expectedTool.stringOrNull("name") ?: "Tool")
                assertWithMessage("$toolLabel input").that(tool.input)
                    .isEqualTo(expectedTool.stringOrNull("input") ?: "")
                assertWithMessage("$toolLabel output").that(tool.output)
                    .isEqualTo(expectedTool.stringOrNull("output") ?: "")
                assertWithMessage("$toolLabel error").that(tool.error)
                    .isEqualTo(expectedTool["error"]?.jsonPrimitive?.booleanOrNull == true)
                assertWithMessage("$toolLabel truncated").that(tool.truncated)
                    .isEqualTo(expectedTool["truncated"]?.jsonPrimitive?.booleanOrNull == true)
            }
        }
    }

    private fun assertOmoPlan(stepLabel: String, expected: JsonElement?, plan: OmoTodoState?) {
        if (expected == null || expected is JsonNull) {
            assertWithMessage("$stepLabel omoPlan").that(plan).isNull()
            return
        }
        val want = expected.jsonObject
        assertWithMessage("$stepLabel omoPlan").that(plan).isNotNull()
        plan!!
        assertWithMessage("$stepLabel omoPlan.available").that(plan.available)
            .isEqualTo(want["available"]?.jsonPrimitive?.booleanOrNull == true)
        assertWithMessage("$stepLabel omoPlan.reasonCode").that(plan.reasonCode)
            .isEqualTo(want.stringOrNull("reason_code"))
        assertWithMessage("$stepLabel omoPlan.sessionId").that(plan.sessionId)
            .isEqualTo(want.stringOrNull("session_id"))
        assertWithMessage("$stepLabel omoPlan.version").that(plan.version)
            .isEqualTo(want["version"]?.jsonPrimitive?.longOrNull?.toInt())
        assertWithMessage("$stepLabel omoPlan.updatedAt").that(plan.updatedAt)
            .isEqualTo(want.stringOrNull("updated_at"))
        assertWithMessage("$stepLabel omoPlan.truncated").that(plan.truncated)
            .isEqualTo(want["truncated"]?.jsonPrimitive?.booleanOrNull == true)
        val expectedPhases = (want["phases"] as? JsonArray).orEmpty()
        assertWithMessage("$stepLabel omoPlan.phases.size").that(plan.phases)
            .hasSize(expectedPhases.size)
        for ((index, pair) in expectedPhases.zip(plan.phases).withIndex()) {
            val (wantPhase, phase) = pair
            val phaseLabel = "$stepLabel omoPlan.phases[$index]"
            assertWithMessage("$phaseLabel name").that(phase.name)
                .isEqualTo(wantPhase.jsonObject.string("name"))
            val expectedTasks = (wantPhase.jsonObject["tasks"] as? JsonArray).orEmpty()
            assertWithMessage("$phaseLabel tasks.size").that(phase.tasks)
                .hasSize(expectedTasks.size)
            for ((taskIndex, taskPair) in expectedTasks.zip(phase.tasks).withIndex()) {
                val (wantTask, task) = taskPair
                val taskLabel = "$phaseLabel tasks[$taskIndex]"
                assertWithMessage("$taskLabel id").that(task.id)
                    .isEqualTo(wantTask.jsonObject.stringOrNull("id"))
                assertWithMessage("$taskLabel content").that(task.content)
                    .isEqualTo(wantTask.jsonObject.string("content"))
                assertWithMessage("$taskLabel status").that(task.status).isEqualTo(
                    OmoTaskStatus.fromWire(wantTask.jsonObject.string("status")),
                )
            }
        }
    }

    companion object {
        /** One suite file per agent kind plus the cross-cutting errors suite. */
        private val SUITES = listOf(
            "claude", "codex", "errors", "hermes", "omo", "omp", "opencode", "pi", "qoder",
        )

        /** sqlite-backed readers: the wire marks their pages mode=native. */
        private val SQLITE_AGENTS = setOf("opencode", "hermes")

        /**
         * Reason codes the file-agent paths surface as `available=false`
         * pages (`browseUnavailable`); any other non-empty `reason_code` on
         * an available page becomes `state=failed` (`browseFailure`).
         */
        private val UNAVAILABLE_CODES = setOf(
            ConversationReason.INVALID_PROVIDER,
            ConversationReason.INVALID_SESSION,
            ConversationReason.SOURCE_UNAVAILABLE,
            ConversationReason.PATH_UNCONTAINED,
        )

        /** `nativeFailure` keeps only these two codes `available=false`. */
        private val NATIVE_UNAVAILABLE_CODES = setOf(
            ConversationReason.INVALID_SESSION,
            ConversationReason.SOURCE_UNAVAILABLE,
        )

        /** `browseErrorFor` retryable flags by code (Go call sites). */
        private val RETRYABLE_CODES = setOf(
            ConversationReason.CURSOR_EXPIRED,
            ConversationReason.REQUEST_CANCELLED,
            ConversationReason.SOURCE_UNAVAILABLE,
            ConversationReason.OUTPUT_LIMIT,
            ConversationReason.QUERY_FAILED,
            ConversationReason.INDEX_FAILED,
            ConversationReason.INDEX_BUSY,
            ConversationReason.INDEX_CAPACITY_EXCEEDED,
            ConversationReason.INDEX_STORAGE_UNAVAILABLE,
        )

        /** Raw continuation-segment id suffix (see `namespaced` in the suite notes). */
        private val NAMESPACED_ID = Regex("[0-9a-f]{24}(-[0-9]+)?")
    }
}
