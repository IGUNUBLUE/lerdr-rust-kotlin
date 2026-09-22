package com.lerdr.app.session

import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.longOrNull
import lerdr.core.transport.CommandException

/**
 * Workspace-inspection payload models — the Kotlin mirror of the oracle's
 * `WorkspaceTree`/`WorkspaceFile`/`WorkspaceGitStatus`/`WorkspaceGitDiff`
 * (`frontend/src/lib/types.ts`) as the relay emits them inside
 * `command_result.data` (see `inspect.rs`: serde field names are
 * snake_case, `Option`/`skip_serializing_if` fields are absent when empty).
 */

enum class WorkspaceEntryKind {
    DIRECTORY,
    FILE,
}

/** One `workspace_tree` entry — [path] is workspace-relative, slash-joined. */
data class WorkspaceTreeEntry(
    val path: String,
    val name: String,
    val kind: WorkspaceEntryKind,
    /** Files only — directories carry no size on the wire. */
    val size: Long? = null,
) {
    val isDirectory: Boolean get() = kind == WorkspaceEntryKind.DIRECTORY
}

/** `workspace_tree` result — flat bounded listing (relay caps at 4,000). */
data class WorkspaceTree(
    val root: String,
    val entries: List<WorkspaceTreeEntry>,
    /** True when the relay stopped at its entry cap. */
    val truncated: Boolean = false,
)

enum class WorkspacePreviewKind {
    TEXT,
    IMAGE,
}

/**
 * `workspace_file` result — [text] is populated for [WorkspacePreviewKind.TEXT]
 * (bounded at 1 MiB), [dataUrl] for [WorkspacePreviewKind.IMAGE]
 * (`data:<media>;base64,…`, bounded at 5 MiB).
 */
data class WorkspaceFilePreview(
    val path: String,
    val mediaType: String,
    val kind: WorkspacePreviewKind,
    val text: String = "",
    val dataUrl: String = "",
    val size: Long = 0,
)

/** One porcelain entry — [status] is the raw two-letter `XY` code. */
data class WorkspaceGitFile(
    val path: String,
    /** Rename/copy source — only present for `R`/`C` statuses. */
    val originalPath: String = "",
    val status: String,
)

/** `workspace_git_status` result — [available] false means "not a git repo". */
data class WorkspaceGitStatus(
    val available: Boolean,
    val branch: String = "",
    val ahead: Long? = null,
    val behind: Long? = null,
    val files: List<WorkspaceGitFile> = emptyList(),
    val truncated: Boolean = false,
)

/** `workspace_git_diff` result — staged+unstaged hunks concatenated. */
data class WorkspaceGitDiff(
    val path: String,
    val diff: String,
)

// ── parsing (oracle validation parity — invalid payloads throw CommandException) ──

internal fun parseWorkspaceTree(data: JsonElement?): WorkspaceTree {
    val obj = data as? JsonObject
    val root = obj?.stringField("root")
    val entries = obj?.get("entries") as? kotlinx.serialization.json.JsonArray
    if (root == null || entries == null) {
        throw CommandException("Relay returned an invalid workspace tree.")
    }
    return WorkspaceTree(
        root = root,
        entries = entries.mapNotNull { element ->
            val entry = element as? JsonObject ?: return@mapNotNull null
            val path = entry.stringField("path") ?: return@mapNotNull null
            val name = entry.stringField("name") ?: path.substringAfterLast('/')
            // The oracle renders anything not "directory" as a file row.
            val kind = if (entry.stringField("kind") == "directory") {
                WorkspaceEntryKind.DIRECTORY
            } else {
                WorkspaceEntryKind.FILE
            }
            WorkspaceTreeEntry(
                path = path,
                name = name,
                kind = kind,
                size = entry.longField("size"),
            )
        },
        truncated = obj.booleanField("truncated") == true,
    )
}

internal fun parseWorkspaceFile(data: JsonElement?, path: String): WorkspaceFilePreview {
    val obj = data as? JsonObject
    val kind = when (obj?.stringField("kind")) {
        "text" -> WorkspacePreviewKind.TEXT
        "image" -> WorkspacePreviewKind.IMAGE
        else -> null
    }
    if (obj == null || obj.stringField("path") != path || kind == null) {
        throw CommandException("Relay returned an invalid workspace preview.")
    }
    return WorkspaceFilePreview(
        path = path,
        mediaType = obj.stringField("media_type").orEmpty(),
        kind = kind,
        text = obj.stringField("text").orEmpty(),
        dataUrl = obj.stringField("data_url").orEmpty(),
        size = obj.longField("size") ?: 0,
    )
}

internal fun parseWorkspaceGitStatus(data: JsonElement?): WorkspaceGitStatus {
    val obj = data as? JsonObject
    val available = obj?.booleanField("available")
    val files = obj?.get("files") as? kotlinx.serialization.json.JsonArray
    if (available == null || files == null) {
        throw CommandException("Relay returned an invalid Git status.")
    }
    return WorkspaceGitStatus(
        available = available,
        branch = obj.stringField("branch").orEmpty(),
        ahead = obj.longField("ahead"),
        behind = obj.longField("behind"),
        files = files.mapNotNull { element ->
            val entry = element as? JsonObject ?: return@mapNotNull null
            val path = entry.stringField("path") ?: return@mapNotNull null
            WorkspaceGitFile(
                path = path,
                originalPath = entry.stringField("original_path").orEmpty(),
                status = entry.stringField("status").orEmpty(),
            )
        },
        truncated = obj.booleanField("truncated") == true,
    )
}

internal fun parseWorkspaceGitDiff(data: JsonElement?, path: String): WorkspaceGitDiff {
    val obj = data as? JsonObject
    val diff = obj?.stringField("diff")
    if (obj == null || obj.stringField("path") != path || diff == null) {
        throw CommandException("Relay returned an invalid Git diff.")
    }
    return WorkspaceGitDiff(path = path, diff = diff)
}

private fun JsonObject.stringField(name: String): String? =
    (this[name] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull

private fun JsonObject.longField(name: String): Long? =
    (this[name] as? JsonPrimitive)?.longOrNull

private fun JsonObject.booleanField(name: String): Boolean? =
    (this[name] as? JsonPrimitive)?.booleanOrNull
