package lerdr.core.data

import app.cash.turbine.test
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertThrows
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

@OptIn(ExperimentalCoroutinesApi::class)
class KeystoreCredentialStoreTest {

    @get:Rule
    val tmp: TemporaryFolder = TemporaryFolder()

    /** Shift cipher: stands in for the Keystore wrap — proves bytes at rest are transformed. */
    private class FakeCipher : CredentialCipher {
        override fun seal(plaintext: ByteArray): ByteArray =
            ByteArray(plaintext.size) { (plaintext[it] + 1).toByte() }

        override fun open(sealed: ByteArray): ByteArray =
            ByteArray(sealed.size) { (sealed[it] - 1).toByte() }
    }

    private class BrokenCipher : CredentialCipher {
        override fun seal(plaintext: ByteArray): ByteArray = plaintext
        override fun open(sealed: ByteArray): ByteArray = throw IllegalStateException("key lost")
    }

    private var now = 1_000_000L

    private fun TestScope.store(cipher: CredentialCipher = FakeCipher(), file: File = File(tmp.root, "credentials.dat")) =
        KeystoreCredentialStore(file, cipher, backgroundScope, { now })

    /** Suspend-friendly assertThrows — runBlocking here would deadlock the test scheduler. */
    private suspend fun assertSuspendThrows(
        clazz: kotlin.reflect.KClass<out Throwable>,
        block: suspend () -> Unit,
    ) {
        try {
            block()
        } catch (expected: Throwable) {
            assertThat(expected).isInstanceOf(clazz.java)
            return
        }
        throw AssertionError("expected ${clazz.simpleName}")
    }

    private val secretA = "A".repeat(43)
    private val secretB = "B".repeat(43)

    private fun invitation(
        id: String = "inv-1",
        version: Long = 1,
        expiresAt: Long? = now + 60_000,
    ) = RelayInvitation(id, version, secretA, expiresAt)

    private fun enrollment(
        deviceId: String = "dev-1",
        credentialId: String = "cred-1",
        version: Long = 1,
        secret: ByteArray? = ByteArray(32) { 7 },
        role: DeviceRole = DeviceRole.CONTROLLER,
        locale: String = "en",
    ) = CredentialEnrollment(deviceId, credentialId, version, secret, role, locale)

    // ── invitation lifecycle ─────────────────────────────────────────

    @Test
    fun `get returns null for unknown relay`() = runTest {
        assertThat(store().get("r1")).isNull()
    }

    @Test
    fun `saveInvitation stores and publishes the record`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        val stored = store.get("r1") as RelayInvitation
        assertThat(stored.id).isEqualTo("inv-1")
        assertThat(stored.secret).isEqualTo(secretA)
        assertThat(store.records.value["r1"]).isEqualTo(stored)
    }

    @Test
    fun `saveInvitation rejects an already-expired invitation`() = runTest {
        val store = store()
        assertSuspendThrows(IllegalArgumentException::class) {
            store.saveInvitation("r1", invitation(expiresAt = now - 1))
        }
        assertThat(store.get("r1")).isNull()
    }

    @Test
    fun `expired invitation self-evicts on read`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        now += 120_000
        assertThat(store.get("r1")).isNull()
        assertThat(store.records.value).isEmpty()
    }

    @Test
    fun `bootstrap invitation never expires`() = runTest {
        val store = store()
        val bootstrap = RelayInvitation(ByteArray(32) { it.toByte() })
        store.saveInvitation("r1", bootstrap)
        now += 365L * 24 * 3600 * 1000
        val stored = store.get("r1") as RelayInvitation
        assertThat(stored.id).isEqualTo("bootstrap")
        assertThat(stored.secretBytes()).hasLength(32)
    }

    // ── redemption ───────────────────────────────────────────────────

    @Test
    fun `redeemInvitation swaps invitation for credential atomically`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        val credential = store.redeemInvitation("r1", "inv-1", enrollment())
        assertThat(credential.id).isEqualTo("cred-1")
        assertThat(credential.deviceId).isEqualTo("dev-1")
        assertThat(credential.invitationId).isEqualTo("inv-1")
        assertThat(credential.issuedAtEpochMs).isEqualTo(now)
        assertThat(store.get("r1")).isEqualTo(credential)
    }

    @Test
    fun `redeemInvitation rejects a mismatched invitation id`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        assertSuspendThrows(IllegalStateException::class) {
            store.redeemInvitation("r1", "other", enrollment())
        }
        // The invitation survives a failed redemption.
        assertThat(store.get("r1")).isInstanceOf(RelayInvitation::class.java)
    }

    @Test
    fun `redeemInvitation requires an issued credential secret`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        assertSuspendThrows(IllegalArgumentException::class) {
            store.redeemInvitation("r1", "inv-1", enrollment(secret = null))
        }
    }

    @Test
    fun `redeemInvitation over a live credential is rejected`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        store.redeemInvitation("r1", "inv-1", enrollment())
        assertSuspendThrows(IllegalStateException::class) {
            store.redeemInvitation("r1", "inv-1", enrollment())
        }
    }

    // ── credential refresh ───────────────────────────────────────────

    @Test
    fun `updateCredential refreshes fields and keeps the secret`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        val first = store.redeemInvitation("r1", "inv-1", enrollment())
        val updated = store.updateCredential(
            "r1",
            enrollment(version = 2, secret = null, role = DeviceRole.READER, locale = "de"),
        )
        assertThat(updated.version).isEqualTo(2)
        assertThat(updated.role).isEqualTo(DeviceRole.READER)
        assertThat(updated.locale).isEqualTo("de")
        assertThat(updated.secret).isEqualTo(first.secret)
    }

    @Test
    fun `updateCredential rejects a different device identity`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        store.redeemInvitation("r1", "inv-1", enrollment())
        assertSuspendThrows(IllegalStateException::class) {
            store.updateCredential("r1", enrollment(credentialId = "cred-2", secret = null))
        }
    }

    @Test
    fun `updateCredential rejects a relay re-issuing a secret`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        store.redeemInvitation("r1", "inv-1", enrollment())
        assertSuspendThrows(IllegalArgumentException::class) {
            store.updateCredential("r1", enrollment())
        }
    }

    @Test
    fun `updateCredential with nothing stored is rejected`() = runTest {
        val store = store()
        assertSuspendThrows(IllegalStateException::class) {
            store.updateCredential("r1", enrollment(secret = null))
        }
    }

    // ── remove / clear ───────────────────────────────────────────────

    @Test
    fun `remove drops only the named relay`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        store.saveInvitation("r2", invitation())
        assertThat(store.remove("r1")).isTrue()
        assertThat(store.remove("r1")).isFalse()
        assertThat(store.get("r1")).isNull()
        assertThat(store.get("r2")).isNotNull()
    }

    @Test
    fun `clear wipes every record`() = runTest {
        val store = store()
        store.saveInvitation("r1", invitation())
        store.saveInvitation("r2", invitation())
        store.clear()
        assertThat(store.records.value).isEmpty()
        assertThat(store.get("r1")).isNull()
    }

    // ── sealing & durability ─────────────────────────────────────────

    @Test
    fun `secrets are never plaintext at rest`() = runTest {
        val file = File(tmp.root, "credentials.dat")
        val store = store(file = file)
        store.saveInvitation("r1", invitation())
        store.redeemInvitation("r1", "inv-1", enrollment())

        val bytes = file.readBytes()
        val text = String(bytes, Charsets.ISO_8859_1)
        assertThat(text).doesNotContain("cred-1")
        assertThat(text).doesNotContain("kind")
        assertThat(bytes.copyOfRange(0, 4)).isEqualTo(byteArrayOf('L'.code.toByte(), 'D'.code.toByte(), 'K'.code.toByte(), 'C'.code.toByte()))
    }

    @Test
    fun `records survive a fresh store on the same file`() = runTest {
        val file = File(tmp.root, "credentials.dat")
        val cipher = FakeCipher()
        val first = KeystoreCredentialStore(file, cipher, backgroundScope, { now })
        first.saveInvitation("r1", invitation())
        first.redeemInvitation("r1", "inv-1", enrollment())

        val restored = KeystoreCredentialStore(file, cipher, backgroundScope, { now })
        val credential = restored.get("r1") as RelayDeviceCredential
        assertThat(credential.id).isEqualTo("cred-1")
        assertThat(credential.deviceId).isEqualTo("dev-1")
        assertThat(credential.role).isEqualTo(DeviceRole.CONTROLLER)
        assertThat(credential.secretBytes()).isEqualTo(ByteArray(32) { 7 })
    }

    @Test
    fun `corrupt file reads as empty`() = runTest {
        val file = File(tmp.root, "credentials.dat")
        file.writeBytes(byteArrayOf(1, 2, 3))
        assertThat(store(file = file).get("r1")).isNull()
    }

    @Test
    fun `undecryptable blob reads as empty`() = runTest {
        val file = File(tmp.root, "credentials.dat")
        val writer = store(file = file)
        writer.saveInvitation("r1", invitation())

        val broken = KeystoreCredentialStore(file, BrokenCipher(), backgroundScope, { now })
        assertThat(broken.get("r1")).isNull()
        assertThat(broken.records.value).isEmpty()
    }

    @Test
    fun `records flow republishes on every mutation`() = runTest {
        val store = store()
        store.records.test {
            store.get("r1") // force the initial load emission
            assertThat(awaitItem()).isEmpty()
            store.saveInvitation("r1", invitation())
            assertThat(awaitItem().keys).containsExactly("r1")
            store.remove("r1")
            assertThat(awaitItem()).isEmpty()
            cancelAndIgnoreRemainingEvents()
        }
    }

    // ── model validation ─────────────────────────────────────────────

    @Test
    fun `records validate at construction`() {
        assertThrows(IllegalArgumentException::class.java) {
            RelayInvitation("bad id\u0001", 1, secretA)
        }
        assertThrows(IllegalArgumentException::class.java) {
            RelayInvitation("ok", 0, secretA)
        }
        assertThrows(IllegalArgumentException::class.java) {
            RelayInvitation("ok", 1, "too-short")
        }
        assertThrows(IllegalArgumentException::class.java) {
            RelayInvitation("ok", 1, "A".repeat(44))
        }
        assertThrows(IllegalArgumentException::class.java) {
            RelayDeviceCredential("c", 1, secretA, "d", DeviceRole.READER, "english", now)
        }
        assertThrows(IllegalArgumentException::class.java) {
            RelayDeviceCredential("c", 1, secretA, "d", DeviceRole.READER, "en-US-x", now)
        }
        // A well-formed BCP-47-ish locale is accepted.
        RelayDeviceCredential("c", 1, secretA, "d", DeviceRole.READER, "en-US", now)
    }

    @Test
    fun `toAuthentication bridges into transport handshake material`() {
        val invitation = invitation().toAuthentication("de")
        assertThat(invitation.selector.kind.wireName).isEqualTo("invitation")
        assertThat(invitation.selector.id).isEqualTo("inv-1")
        assertThat(invitation.selector.locale).isEqualTo("de")

        val credential = RelayDeviceCredential("cred-1", 3, secretA, "dev-1", DeviceRole.READER, "fr", now)
            .toAuthentication()
        assertThat(credential.selector.kind.wireName).isEqualTo("credential")
        assertThat(credential.selector.version).isEqualTo(3)
        assertThat(credential.selector.locale).isEqualTo("fr")
    }
}
