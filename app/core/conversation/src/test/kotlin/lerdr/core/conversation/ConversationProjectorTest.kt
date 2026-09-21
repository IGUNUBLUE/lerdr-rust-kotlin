package lerdr.core.conversation

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import lerdr.core.model.CommandResultMessage
import org.junit.Assert.assertThrows
import org.junit.Test

/**
 * Wire-contract invariants for [ConversationProjector] — the same checks the
 * released web client applies in `normalizeConversationPage`
 * (`frontend/src/lib/store.ts`), plus the request-side clamping.
 */
class ConversationProjectorTest {

    private fun pageJson(builder: kotlinx.serialization.json.JsonObjectBuilder.() -> Unit = {}) =
        buildJsonObject {
            put("available", true)
            put("entries", JsonArray(emptyList()))
            put("has_more", false)
            put("total", JsonNull)
            put("state", "ready")
            put("mode", "recent")
            builder()
        }

    private fun project(page: JsonObject) = ConversationProjector.project(page)

    @Test
    fun `minimal ready page`() {
        val page = project(pageJson { put("total", 0) })
        assertThat(page.available).isTrue()
        assertThat(page.state).isEqualTo(ConversationBrowseState.READY)
        assertThat(page.mode).isEqualTo(ConversationBrowseMode.RECENT)
        assertThat(page.entries).isEmpty()
        assertThat(page.nextCursor).isEmpty()
        assertThat(page.hasMore).isFalse()
        assertThat(page.total).isEqualTo(0)
        assertThat(page.progress).isNull()
        assertThat(page.error).isNull()
        assertThat(page.omoPlan).isNull()
        assertThat(page.diagnostics).isEqualTo(ConversationDiagnostics())
    }

    @Test
    fun `accepts CommandResultMessage data`() {
        val result = CommandResultMessage(
            action = ConversationPageRequest.ACTION,
            data = pageJson { put("total", 3) },
            ok = true,
        )
        assertThat(ConversationProjector.project(result).total).isEqualTo(3)
    }

    @Test
    fun `rejects non-object payload`() {
        assertThrows(ConversationProjectionException::class.java) {
            ConversationProjector.project(JsonPrimitive("page"))
        }
        assertThrows(ConversationProjectionException::class.java) {
            ConversationProjector.project(null)
        }
    }

    @Test
    fun `rejects missing required fields`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(buildJsonObject { put("entries", JsonArray(emptyList())) })
        }
        for (field in listOf("available", "entries", "has_more", "state", "mode")) {
            val broken = buildJsonObject {
                pageJson().forEach { (k, v) -> if (k != field) put(k, v) }
            }
            assertThrows("'$field' absent", ConversationProjectionException::class.java) {
                project(broken)
            }
        }
    }

    @Test
    fun `rejects stringly booleans`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("has_more", "true") })
        }
    }

    @Test
    fun `rejects unknown state and mode`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("state", "loading") })
        }
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("mode", "deep") })
        }
    }

    @Test
    fun `has_more requires next_cursor`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("has_more", true) })
        }
        val page = project(pageJson {
            put("has_more", true)
            put("next_cursor", "hb1.payload")
        })
        assertThat(page.nextCursor).isEqualTo("hb1.payload")
        assertThat(page.hasMore).isTrue()
    }

    @Test
    fun `preparing requires next_cursor and may carry progress`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("state", "preparing") })
        }
        val page = project(pageJson {
            put("state", "preparing")
            put("has_more", true)
            put("next_cursor", "hb1.payload")
            put("progress", buildJsonObject {
                put("phase", "scan")
                put("scanned_bytes", 10)
                put("source_bytes", 42)
            })
        })
        assertThat(page.state).isEqualTo(ConversationBrowseState.PREPARING)
        assertThat(page.progress).isEqualTo(
            ConversationBrowseProgress(phase = "scan", scannedBytes = 10, sourceBytes = 42),
        )
    }

    @Test
    fun `progress rejects scanned beyond source`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson {
                put("state", "preparing")
                put("next_cursor", "hb1.x")
                put("progress", buildJsonObject {
                    put("phase", "scan")
                    put("scanned_bytes", 43)
                    put("source_bytes", 42)
                })
            })
        }
    }

    @Test
    fun `failed requires error`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("state", "failed") })
        }
        val page = project(pageJson {
            put("state", "failed")
            put("reason_code", "invalid_cursor")
            put("error", buildJsonObject {
                put("code", "invalid_cursor")
                put("message", "bad cursor")
                put("retryable", false)
            })
        })
        assertThat(page.error).isEqualTo(
            ConversationBrowseError("invalid_cursor", "bad cursor", retryable = false),
        )
    }

    @Test
    fun `entries cap and malformed entry filtering`() {
        val oversized = buildJsonArray { repeat(201) { add(entryJson("e$it")) } }
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("entries", oversized) })
        }

        val entries = buildJsonArray {
            add(entryJson("keep-1"))
            add(JsonPrimitive("junk"))
            add(entryJson(""))                       // empty id
            add(entryJson("no-role", role = null))
            add(entryJson("empty", text = ""))       // neither text nor tools
            add(entryJson("keep-2", text = "hi"))
        }
        val page = project(pageJson { put("entries", entries) })
        assertThat(page.entries.map { it.id }).containsExactly("keep-1", "keep-2").inOrder()
    }

    @Test
    fun `entry fields map and tools default`() {
        val entry = entryJson(
            "id-1",
            role = "assistant",
            text = "running",
            tools = buildJsonArray {
                add(buildJsonObject {
                    put("id", "toolu_1")
                    put("name", "Bash")
                    put("input", "{\"command\":\"ls\"}")
                    put("output", "ok")
                    put("error", true)
                    put("truncated", true)
                })
                add(buildJsonObject { put("name", "Read") })
                add(JsonPrimitive("junk"))
            },
        )
        val page = project(pageJson { put("entries", buildJsonArray { add(entry) }) })
        val projected = page.entries.single()
        assertThat(projected.role).isEqualTo(ConversationRole.ASSISTANT)
        assertThat(projected.truncated).isFalse()
        assertThat(projected.tools[0]).isEqualTo(
            ConversationTool(
                id = "toolu_1", name = "Bash",
                input = "{\"command\":\"ls\"}", output = "ok",
                error = true, truncated = true,
            ),
        )
        assertThat(projected.tools[1]).isEqualTo(ConversationTool(name = "Read"))
        assertThat(projected.tools).hasSize(2)
    }

    @Test
    fun `tool defaults name and tolerates missing fields`() {
        val entry = entryJson(
            "id-1",
            tools = buildJsonArray { add(buildJsonObject { put("output", "x") }) },
        )
        val tool = project(pageJson { put("entries", buildJsonArray { add(entry) }) })
            .entries.single().tools.single()
        assertThat(tool.name).isEqualTo("Tool")
        assertThat(tool.id).isEmpty()
        assertThat(tool.error).isFalse()
    }

    @Test
    fun `diagnostics invariants`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson {
                put("diagnostics", buildJsonObject { put("continuation_reason", "cycle") })
            })
        }
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson {
                put("diagnostics", buildJsonObject {
                    put("continuation_incomplete", true)
                    put("continuation_reason", "bogus")
                })
            })
        }
        val page = project(pageJson {
            put("diagnostics", buildJsonObject {
                put("continuation_incomplete", true)
                put("continuation_reason", "cycle")
                put("corrupt_records", 2)
                put("source_truncated", true)
            })
        })
        assertThat(page.diagnostics.continuationReason).isEqualTo("cycle")
        assertThat(page.sourceCorrupt).isTrue()
        assertThat(page.fileTruncated).isTrue()
    }

    @Test
    fun `omo plan null and malformed rows`() {
        val plan = buildJsonObject {
            put("available", true)
            put("session_id", "s1")
            put("version", 2)
            put("phases", buildJsonArray {
                add(buildJsonObject {
                    put("name", "P1")
                    put("tasks", buildJsonArray {
                        add(buildJsonObject {
                            put("id", "t1")
                            put("content", "do it")
                            put("status", "in_progress")
                        })
                        add(buildJsonObject {
                            put("content", "")
                            put("status", "bogus")
                        })
                    })
                })
            })
        }
        val page = project(pageJson { put("omo_plan", plan) })
        val omo = page.omoPlan!!
        assertThat(omo.available).isTrue()
        assertThat(omo.sessionId).isEqualTo("s1")
        assertThat(omo.version).isEqualTo(2)
        assertThat(omo.phases.single().tasks).containsExactly(
            OmoTodoTask(id = "t1", content = "do it", status = OmoTaskStatus.IN_PROGRESS),
        )
    }

    @Test
    fun `total must be a non-negative integer`() {
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("total", -1) })
        }
        assertThrows(ConversationProjectionException::class.java) {
            project(pageJson { put("total", "3") })
        }
    }

    @Test
    fun `unknown fields are ignored`() {
        val page = project(pageJson {
            put("total", 1)
            put("future_field", buildJsonObject { put("x", 1) })
        })
        assertThat(page.total).isEqualTo(1)
    }

    @Test
    fun `request limit clamps to relay bounds`() {
        assertThat(ConversationPageRequest(limit = 0).effectiveLimit)
            .isEqualTo(ConversationPageRequest.DEFAULT_PAGE_SIZE)
        assertThat(ConversationPageRequest(limit = -5).effectiveLimit)
            .isEqualTo(ConversationPageRequest.DEFAULT_PAGE_SIZE)
        assertThat(ConversationPageRequest(limit = 10_000).effectiveLimit)
            .isEqualTo(ConversationPageRequest.MAX_PAGE_SIZE)
        assertThat(ConversationPageRequest(limit = 37).effectiveLimit).isEqualTo(37)
        assertThat(ConversationPageRequest.DEFAULT_PAGE_SIZE).isEqualTo(80)
        assertThat(ConversationPageRequest.MAX_PAGE_SIZE).isEqualTo(200)
    }

    private fun entryJson(
        id: String,
        role: String? = "user",
        text: String = "body",
        tools: JsonElement? = null,
    ): JsonObject = buildJsonObject {
        put("id", id)
        role?.let { put("role", it) }
        put("text", text)
        put("timestamp", "2026-08-12T10:00:00Z")
        tools?.let { put("tools", it) }
    }
}
