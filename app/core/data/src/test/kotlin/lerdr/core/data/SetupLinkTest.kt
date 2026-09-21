package lerdr.core.data

import com.google.common.truth.Truth.assertThat
import org.junit.Test

class SetupLinkTest {

    private val token = "0123456789abcdef0123456789abcdef" // 32-char bootstrap key
    private val secret = "A".repeat(43)                      // valid invitation secret
    private val inviteId = "a".repeat(24)                    // valid invitation id

    private fun parse(text: String?) = SetupLink.parse(text)
    private fun parsed(text: String) =
        (parse(text) as? SetupLinkResult.Parsed)?.payload ?: error("expected Parsed for: $text")

    // ── not a link ───────────────────────────────────────────────────

    @Test
    fun `blank input is Empty`() {
        assertThat(parse(null)).isEqualTo(SetupLinkResult.Empty)
        assertThat(parse("   ")).isEqualTo(SetupLinkResult.Empty)
    }

    @Test
    fun `non-url input is Malformed`() {
        assertThat(parse("not a link")).isEqualTo(SetupLinkResult.Malformed)
        assertThat(parse("ftp://host/#setup=$token")).isEqualTo(SetupLinkResult.Malformed)
    }

    @Test
    fun `missing fragment is Malformed`() {
        assertThat(parse("https://relay.example.com/")).isEqualTo(SetupLinkResult.Malformed)
        assertThat(parse("https://relay.example.com/#")).isEqualTo(SetupLinkResult.Malformed)
    }

    // ── bootstrap (token) links ──────────────────────────────────────

    @Test
    fun `https page link defaults the socket origin to its own host`() {
        val payload = parsed("https://relay.example.com/app#setup=$token&label=Desk")
        assertThat(payload.socketOrigin).isEqualTo("wss://relay.example.com")
        assertThat(payload.label).isEqualTo("Desk")
        assertThat(payload.invitation).isNull()
        assertThat(payload.source).isEqualTo(InvitePayload.Source.PAGE_LINK)
    }

    @Test
    fun `http page link yields a ws origin`() {
        val payload = parsed("http://192.168.1.10:3000/#setup=$token")
        assertThat(payload.socketOrigin).isEqualTo("ws://192.168.1.10:3000")
        assertThat(payload.label).isEqualTo("This computer")
    }

    @Test
    fun `relay param overrides the link host`() {
        val payload = parsed("https://page.example.com/#setup=$token&relay=wss://other.example.com:8443")
        assertThat(payload.socketOrigin).isEqualTo("wss://other.example.com:8443")
    }

    @Test
    fun `ws relay param is refused on an https page`() {
        assertThat(parse("https://page.example.com/#setup=$token&relay=ws://host"))
            .isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `ws relay param is accepted on an http page`() {
        val payload = parsed("http://page.example.com/#setup=$token&relay=ws://host:8080")
        assertThat(payload.socketOrigin).isEqualTo("ws://host:8080")
    }

    @Test
    fun `relay param rejects credentials path query and fragment`() {
        assertThat(parse("https://h/#setup=$token&relay=wss://user:pw@h")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$token&relay=wss://h/p")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$token&relay=wss://h?x=1")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$token&relay=wss://h%23f")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$token&relay=https://h")).isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `gateway params are rejected`() {
        assertThat(parse("https://h/#setup=$token&gateway=wss://g")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$token&gateways=wss://g")).isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `setup token length is bounded`() {
        assertThat(parse("https://h/#setup=short")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=${"x".repeat(513)}")).isEqualTo(SetupLinkResult.NoSetup)
        parsed("https://h/#setup=${"x".repeat(16)}")
        parsed("https://h/#setup=${"x".repeat(512)}")
    }

    @Test
    fun `missing setup param is NoSetup`() {
        assertThat(parse("https://h/#label=x")).isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `label is trimmed and capped at 48 chars`() {
        val payload = parsed("https://h/#setup=$token&label=${"l".repeat(80)}")
        assertThat(payload.label).hasLength(48)
        val blank = parsed("https://h/#setup=$token&label=%20%20")
        assertThat(blank.label).isEqualTo("This computer")
    }

    @Test
    fun `bootstrap payload converts to a bootstrap invitation`() {
        val payload = parsed("https://h/#setup=$token")
        val pending = payload.toPendingInvitation()
        assertThat(pending.id).isEqualTo("bootstrap")
        assertThat(pending.version).isEqualTo(1)
        assertThat(pending.isExpired(Long.MAX_VALUE)).isFalse()
        assertThat(pending.secretBytes()).isEqualTo(token.toByteArray(Charsets.UTF_8))
        assertThat(payload.relayEndpoint()!!.paired).isFalse()
    }

    // ── lerdr:// links ───────────────────────────────────────────────

    @Test
    fun `lerdr link requires a relay param`() {
        assertThat(parse("lerdr://pair#setup=$token")).isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `lerdr link parses with a relay param`() {
        val payload = parsed("lerdr://pair#setup=$token&relay=wss://desk.local&label=Desk")
        assertThat(payload.source).isEqualTo(InvitePayload.Source.LERDR_LINK)
        assertThat(payload.socketOrigin).isEqualTo("wss://desk.local")
        assertThat(payload.label).isEqualTo("Desk")
    }

    @Test
    fun `lerdr link accepts ws relays`() {
        // A native app has no mixed-content rule — LAN ws pairing is valid.
        val payload = parsed("lerdr://pair#setup=$token&relay=ws://192.168.1.5:7777")
        assertThat(payload.socketOrigin).isEqualTo("ws://192.168.1.5:7777")
    }

    // ── invitation links ─────────────────────────────────────────────

    @Test
    fun `full invitation link parses every field`() {
        val expires = 1_800_000_000_000L
        val payload = parsed(
            "https://app.example.com/#setup=$secret&invite=$inviteId" +
                "&invite_version=1&invite_expires=$expires&label=Workstation&relay=wss://ws.example.com",
        )
        val invitation = payload.invitation!!
        assertThat(invitation.id).isEqualTo(inviteId)
        assertThat(invitation.version).isEqualTo(1)
        assertThat(invitation.secret).isEqualTo(secret)
        assertThat(invitation.expiresAtEpochMs).isEqualTo(expires)
        assertThat(payload.socketOrigin).isEqualTo("wss://ws.example.com")
        assertThat(payload.relayEndpoint()!!.paired).isTrue()

        val pending = payload.toPendingInvitation()
        assertThat(pending.id).isEqualTo(inviteId)
        assertThat(pending.expiresAtEpochMs).isEqualTo(expires)
    }

    @Test
    fun `malformed invitation fields reject the whole link`() {
        // No silent downgrade to a token import when invite= is present.
        assertThat(parse("https://h/#setup=$secret&invite=$inviteId")).isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$secret&invite=$inviteId&invite_version=0&invite_expires=5"))
            .isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$secret&invite=$inviteId&invite_version=1&invite_expires=abc"))
            .isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=notasecretnotasecret&invite=$inviteId&invite_version=1&invite_expires=5"))
            .isEqualTo(SetupLinkResult.NoSetup)
        assertThat(parse("https://h/#setup=$secret&invite=x&invite_version=1&invite_expires=5"))
            .isEqualTo(SetupLinkResult.NoSetup)
        // An invite id of 129 chars exceeds the cap.
        assertThat(
            parse("https://h/#setup=$secret&invite=${"a".repeat(129)}&invite_version=1&invite_expires=5"),
        ).isEqualTo(SetupLinkResult.NoSetup)
    }

    @Test
    fun `encoded params decode like URLSearchParams`() {
        val payload = parsed("https://h/#setup=$token&label=My+Work%20Box")
        assertThat(payload.label).isEqualTo("My Work Box")
    }
}
