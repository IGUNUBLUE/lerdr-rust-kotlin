package com.lerdr.app.session.feed

import com.google.common.truth.Truth.assertThat
import kotlinx.serialization.json.Json
import org.junit.Test

/**
 * The oracle's `loadSlashCommands` payload mapping + `TerminalView.svelte`
 * filter/keyboard semantics, exercised as pure functions.
 */
class FeedSlashCommandsTest {

    private fun catalogJson(body: String) = Json.parseToJsonElement(
        """{"commands":[$body]}""",
    )

    @Test
    fun `invalid command names are dropped before mapping`() {
        val catalog = parseSlashCatalog(
            catalogJson(
                """{"command":"/ok","description":"fine"},
                   {"command":"no-slash","description":"bad"},
                   {"command":"/","description":"bad"},
                   {"command":"/UPPER_ok","description":"fine"}""",
            ),
        )
        assertThat(catalog.commands.map { it.command })
            .containsExactly("/ok", "/UPPER_ok")
            .inOrder() // case-insensitive sort: U < o? — see below
    }

    @Test
    fun `entries map description hint and normalized source`() {
        val catalog = parseSlashCatalog(
            catalogJson(
                """{"command":"/run","description":"","argument_hint":"<cmd>","source":"project"},
                   {"command":"/help","source":"unknown-vendor"}""",
            ),
        )
        val run = catalog.commands.first { it.command == "/run" }
        // Empty description falls back to the command itself (oracle `||`).
        assertThat(run.description).isEqualTo("/run")
        assertThat(run.argumentHint).isEqualTo("<cmd>")
        assertThat(run.source).isEqualTo("project")
        val help = catalog.commands.first { it.command == "/help" }
        assertThat(help.source).isEqualTo("builtin")
        assertThat(help.argumentHint).isNull()
    }

    @Test
    fun `catalog sorts case-insensitively`() {
        val catalog = parseSlashCatalog(
            catalogJson(
                """{"command":"/zebra"},{"command":"/Alpha"},{"command":"/mid"}""",
            ),
        )
        assertThat(catalog.commands.map { it.command })
            .containsExactly("/Alpha", "/mid", "/zebra")
            .inOrder()
    }

    @Test
    fun `truncated follows the payload flag and the entry cap`() {
        assertThat(
            parseSlashCatalog(
                Json.parseToJsonElement("""{"commands":[],"truncated":true}"""),
            ).truncated,
        ).isTrue()
        assertThat(
            parseSlashCatalog(catalogJson("""{"command":"/a"}""")).truncated,
        ).isFalse()
    }

    @Test
    fun `slashQueryFor opens only on a slash token without whitespace`() {
        assertThat(slashQueryFor("/")).isEqualTo("")
        assertThat(slashQueryFor("/cle")).isEqualTo("cle")
        assertThat(slashQueryFor("/CLEAR")).isEqualTo("clear")
        assertThat(slashQueryFor("/clear now")).isNull()
        assertThat(slashQueryFor("plain")).isNull()
        assertThat(slashQueryFor("")).isNull()
    }

    @Test
    fun `matchingSlashCommands prefix-filters on the name`() {
        val catalog = SlashCommandCatalog(
            listOf(
                SlashCommand("/clear", "Clear the feed"),
                SlashCommand("/close", "Close"),
                SlashCommand("/help", "Show help"),
            ),
        )
        assertThat(matchingSlashCommands(catalog, null)).isEmpty()
        assertThat(matchingSlashCommands(catalog, "")).hasSize(3)
        assertThat(matchingSlashCommands(catalog, "cl").map { it.command })
            .containsExactly("/clear", "/close")
        assertThat(matchingSlashCommands(catalog, "zzz")).isEmpty()
    }

    @Test
    fun `effectiveSlashIndex clamps or reports none`() {
        assertThat(effectiveSlashIndex(0, 0)).isEqualTo(-1)
        assertThat(effectiveSlashIndex(5, 3)).isEqualTo(2)
        assertThat(effectiveSlashIndex(-2, 3)).isEqualTo(0)
        assertThat(effectiveSlashIndex(1, 3)).isEqualTo(1)
    }

    @Test
    fun `selection inserts the command plus a space when hinted`() {
        assertThat(
            slashSelectionText(SlashCommand("/run", "Run", argumentHint = "<cmd>")),
        ).isEqualTo("/run ")
        assertThat(slashSelectionText(SlashCommand("/help", "Help"))).isEqualTo("/help")
    }
}
