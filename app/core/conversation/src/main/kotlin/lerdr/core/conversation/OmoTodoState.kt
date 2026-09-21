package lerdr.core.conversation

/**
 * OMO's structured plan — `OMOTodoState` (`internal/conversation/omo.go`),
 * carried as `omo_plan` on pages served for omo sessions. Sourced from the
 * newest valid `senpi.todo-state` custom record in the transcript.
 */

/** Task lifecycle states the plan schema accepts (`data.schema = "v2"`). */
enum class OmoTaskStatus(val wire: String) {
    PENDING("pending"),
    IN_PROGRESS("in_progress"),
    COMPLETED("completed"),
    ABANDONED("abandoned"),
    ;

    companion object {
        fun fromWire(value: String): OmoTaskStatus? = when (value) {
            "pending" -> PENDING
            "in_progress" -> IN_PROGRESS
            "completed" -> COMPLETED
            "abandoned" -> ABANDONED
            else -> null
        }
    }
}

data class OmoTodoTask(
    val id: String? = null,
    val content: String,
    val status: OmoTaskStatus,
)

data class OmoTodoPhase(
    val name: String,
    val tasks: List<OmoTodoTask> = emptyList(),
)

/**
 * @param available false when no valid plan could be read ([reasonCode] set,
 *   e.g. `source_corrupt` for a malformed todo-state row).
 * @param version plan schema version (2 for `v2` rows); null when no valid
 *   plan row exists.
 * @param truncated true when phase/task caps or the read window clipped rows.
 */
data class OmoTodoState(
    val available: Boolean,
    val reasonCode: String? = null,
    val sessionId: String? = null,
    val version: Int? = null,
    val updatedAt: String? = null,
    val phases: List<OmoTodoPhase> = emptyList(),
    val truncated: Boolean = false,
)
