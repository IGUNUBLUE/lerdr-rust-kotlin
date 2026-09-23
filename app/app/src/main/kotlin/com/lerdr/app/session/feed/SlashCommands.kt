package com.lerdr.app.session.feed

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import com.lerdr.core.designsystem.theme.LerdrTheme
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull

/**
 * Slash-command suggestions — the feed port of the oracle's
 * `TerminalView.svelte` popover, backed by the `list_slash_commands` wire
 * action (gated on the relay's `slash_commands` capability). Parsing and
 * filtering are pure functions so the ViewModel/screens stay testable.
 */

/** One catalog row — oracle `SlashCommand`. */
@Immutable
data class SlashCommand(
    /** `/name` including the leading slash, already wire-validated. */
    val command: String,
    /** Display text — the command itself when the relay sent no summary. */
    val description: String,
    /** Shown after the name; selection inserts a trailing space when set. */
    val argumentHint: String? = null,
    /** `builtin` | `personal` | `project` — anything else reads builtin. */
    val source: String = "builtin",
)

/** Parsed `list_slash_commands` payload. */
@Immutable
data class SlashCommandCatalog(
    val commands: List<SlashCommand> = emptyList(),
    /** Relay hit its discovery cap — suggestions may be incomplete. */
    val truncated: Boolean = false,
)

/**
 * The shared contract cap — `contracts/fixtures/slash_command_limits.json`
 * (`max_entries`). The relay owns the filesystem budget; the client keeps
 * the validation cap in step.
 */
internal const val SLASH_COMMAND_MAX_ENTRIES = 4_096

/** Oracle `MAX_VISIBLE_SLASH_COMMANDS` — the popover renders at most this many. */
internal const val MAX_VISIBLE_SLASH_COMMANDS = 200

/** Wire shape: `/name` — letter/digit start, `.`/`_`/`:`/`-` tail, ≤120. */
internal val SLASH_COMMAND_WIRE = Regex("^/[A-Za-z0-9][A-Za-z0-9._:-]{0,119}$")

private val SLASH_SOURCES = setOf("builtin", "personal", "project")

/**
 * `loadSlashCommands`'s payload mapper — filters invalid `command` values,
 * clamps to [SLASH_COMMAND_MAX_ENTRIES], normalizes descriptions/hints and
 * the source enum, sorts case-insensitively, and reports the catalog's
 * `truncated` flag (also set when the cap clipped valid entries).
 */
internal fun parseSlashCatalog(data: JsonElement?): SlashCommandCatalog {
    val commandsList = (data as? JsonObject)?.get("commands")
        as? kotlinx.serialization.json.JsonArray ?: emptyList()
    val valid = commandsList.mapNotNull { entry ->
        val obj = entry as? JsonObject ?: return@mapNotNull null
        val command = (obj["command"] as? JsonPrimitive)?.contentOrNull
            ?: return@mapNotNull null
        if (!SLASH_COMMAND_WIRE.matches(command)) return@mapNotNull null
        obj to command
    }
    val commands = valid
        .take(SLASH_COMMAND_MAX_ENTRIES)
        .map { (obj, command) ->
            val description = (obj["description"] as? JsonPrimitive)?.contentOrNull
                ?.take(240)?.ifEmpty { null } ?: command
            val hint = (obj["argument_hint"] as? JsonPrimitive)?.contentOrNull
                ?.take(120)?.takeIf { it.isNotEmpty() }
            val source = (obj["source"] as? JsonPrimitive)?.contentOrNull
            SlashCommand(
                command = command,
                description = description,
                argumentHint = hint,
                source = if (source in SLASH_SOURCES) source!! else "builtin",
            )
        }
        .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.command })
    // `Boolean(data.truncated)` — JS truthiness: false/0/absent → false,
    // any non-empty string ("false" included) or object → true.
    val flag = (data as? JsonObject)?.get("truncated")
    val truncated = valid.size > SLASH_COMMAND_MAX_ENTRIES || when (flag) {
        null, kotlinx.serialization.json.JsonNull -> false
        is JsonPrimitive -> if (flag.isString) {
            flag.content.isNotEmpty()
        } else {
            flag.content.toBooleanStrictOrNull()
                ?: (flag.contentOrNull?.toDoubleOrNull()?.let { it != 0.0 } == true)
        }
        else -> true
    }
    return SlashCommandCatalog(commands = commands, truncated = truncated)
}

/**
 * Oracle `slashQuery` — the composer must be `/token` with no whitespace.
 * Returns null when suggestions stay closed; `""` (bare `/`) matches all.
 */
internal fun slashQueryFor(draft: String): String? {
    if (!draft.startsWith("/") || draft.any { it.isWhitespace() }) return null
    return draft.drop(1).lowercase()
}

/** Oracle `matchingSlashCommands` — case-insensitive prefix on the name. */
internal fun matchingSlashCommands(
    catalog: SlashCommandCatalog,
    query: String?,
): List<SlashCommand> {
    if (query == null) return emptyList()
    if (query.isEmpty()) return catalog.commands
    return catalog.commands.filter { entry ->
        entry.command.drop(1).lowercase().startsWith(query)
    }
}

/** Oracle `effectiveSlashIndex` — clamps the active row into the list. */
internal fun effectiveSlashIndex(activeIndex: Int, filteredSize: Int): Int =
    if (filteredSize > 0) activeIndex.coerceIn(0, filteredSize - 1) else -1

/**
 * Oracle `selectSlashCommand` — the command plus a trailing space when the
 * command takes an argument; the cursor lands at the end either way.
 */
internal fun slashSelectionText(command: SlashCommand): String =
    command.command + if (command.argumentHint != null) " " else ""

/**
 * The suggestion popover — header + rows + the oracle's status lines.
 * [activeIndex] is the keyboard-highlighted row ([effectiveSlashIndex]).
 */
@Composable
internal fun SlashCommandMenu(
    commands: List<SlashCommand>,
    matchCount: Int,
    loading: Boolean,
    unavailable: Boolean,
    truncated: Boolean,
    activeIndex: Int,
    onSelect: (SlashCommand) -> Unit,
    modifier: Modifier = Modifier,
) {
    val spacing = LerdrTheme.spacing
    val listState = rememberLazyListState()
    val hidden = matchCount > commands.size
    LaunchedEffect(activeIndex, commands.size) {
        if (activeIndex >= 0 && activeIndex < commands.size) {
            listState.scrollToItem(activeIndex)
        }
    }
    Surface(
        color = MaterialTheme.colorScheme.surfaceContainerLow,
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = MaterialTheme.shapes.medium,
        tonalElevation = 3.dp,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(vertical = spacing.extraSmall)) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(
                    horizontal = spacing.small,
                    vertical = spacing.extraSmall,
                ),
            ) {
                Text(
                    "Commands",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.weight(1f),
                )
                if (!loading && !unavailable) {
                    Text(
                        "$matchCount${if (hidden) "+" else ""} matching",
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            when {
                loading -> SlashStatus("Loading commands…")
                unavailable -> SlashStatus(
                    "Suggestions unavailable — you can still send this command.",
                )
                commands.isEmpty() -> SlashStatus(
                    "No matching command — you can still send it.",
                )
                else -> LazyColumn(
                    state = listState,
                    modifier = Modifier.heightIn(max = 240.dp),
                ) {
                    itemsIndexed(
                        commands,
                        key = { _, entry -> entry.command },
                    ) { index, entry ->
                        SlashCommandRow(
                            entry = entry,
                            active = index == activeIndex,
                            onSelect = { onSelect(entry) },
                        )
                    }
                }
            }
            if (!loading && truncated) {
                SlashStatus(
                    "Command suggestions may be incomplete because a discovery " +
                        "limit was reached. Typing searches only loaded " +
                        "suggestions; you can still send a command manually.",
                )
            }
            if (!loading && hidden) {
                SlashStatus(
                    "More matching commands are hidden; keep typing to narrow the list.",
                )
            }
        }
    }
}

@Composable
private fun SlashStatus(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(
            horizontal = LerdrTheme.spacing.small,
            vertical = LerdrTheme.spacing.extraSmall,
        ),
    )
}

/** One suggestion row — name + argument hint, description, source em. */
@Composable
private fun SlashCommandRow(
    entry: SlashCommand,
    active: Boolean,
    onSelect: () -> Unit,
) {
    val spacing = LerdrTheme.spacing
    Surface(
        onClick = onSelect,
        color = if (active) {
            MaterialTheme.colorScheme.surfaceContainerHigh
        } else {
            MaterialTheme.colorScheme.surfaceContainerLow
        },
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(
            verticalArrangement = Arrangement.spacedBy(2.dp),
            modifier = Modifier
                .fillMaxWidth()
                .padding(
                    horizontal = spacing.small,
                    vertical = spacing.extraSmall,
                ),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    entry.command,
                    style = MaterialTheme.typography.bodyMedium,
                    fontWeight = FontWeight.SemiBold,
                )
                entry.argumentHint?.let { hint ->
                    Text(
                        " $hint",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Text(
                entry.description,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            if (entry.source != "builtin") {
                Text(
                    entry.source,
                    style = MaterialTheme.typography.labelSmall,
                    fontStyle = FontStyle.Italic,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
