package com.lerdr.app.session.feed

import lerdr.core.conversation.ConversationEntry

/**
 * Feed find — the conversation analogue of the terminal's
 * `terminal-find.ts`. The oracle (`ConversationHistory.svelte`) filters
 * entries on one joined corpus per row and highlights the needle inside
 * each rendered message; result navigation reuses the terminal find bar
 * (`TerminalFindBar` in `com.lerdr.app.session`) where one "match" is one
 * matching entry row.
 */

/**
 * Oracle corpus — `${entry.text} ${tool.name} ${tool.input} ${tool.output}`
 * joined over the row's tools.
 */
internal fun feedEntrySearchText(entry: ConversationEntry): String = buildString {
    append(entry.text)
    entry.tools.forEach { tool ->
        append(' ')
        append(tool.name)
        append(' ')
        append(tool.input)
        append(' ')
        append(tool.output)
    }
}

/**
 * `visibleEntries` — indices (into [entries]) whose corpus contains
 * [needle], case-insensitive. An empty needle matches everything, so the
 * caller can use the result both as the filtered list and the match map.
 */
internal fun feedMatchingEntryIndexes(
    entries: List<ConversationEntry>,
    needle: String,
): List<Int> {
    val trimmed = needle.trim()
    if (trimmed.isEmpty()) return entries.indices.toList()
    return entries.indices.filter { index ->
        feedEntrySearchText(entries[index]).contains(trimmed, ignoreCase = true)
    }
}
