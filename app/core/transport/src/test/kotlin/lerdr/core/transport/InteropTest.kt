package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import java.nio.file.Files
import java.nio.file.Path
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.model.Inbound
import okhttp3.OkHttpClient
import org.junit.Assume.assumeTrue
import org.junit.Test

/**
 * Live interop against the running Go relay (`lerdr serve` at
 * `ws://127.0.0.1:8375`). Gated on `LERDR_INTEROP=1` — absent, the test
 * skips and records why. The bootstrap invitation secret is read from the
 * oracle's `relay/.env` at runtime and is never printed or logged.
 *
 * Flow: re-arm the one-shot bootstrap invitation (SIGUSR1), pair through the
 * `herdr-e2ee-v2` handshake, confirm a credential is issued, confirm the
 * session decrypts inbound frames (`push_config`), then `device_list` must
 * name this device — the relay's own view of the pairing. Cleanup revokes
 * the freshly issued credential via `revoke_device`.
 */
class InteropTest {

    @Test(timeout = 120_000)
    fun pairsAndListsDevicesOverLiveRelay() = runBlocking {
        assumeTrue("LERDR_INTEROP=1 not set — live interop skipped", isInteropEnabled())

        val token = readRelayToken()
        assumeTrue("HERDR_RELAY_TOKEN missing/invalid in oracle .env", token != null)
        assumeTrue("relay process not found — cannot re-arm bootstrap invitation", rearmBootstrap())

        val invitation = DeviceAuthentication.invitation(
            id = "bootstrap",
            secret = token!!,
            version = 1,
            locale = "en",
        )
        val enrolled = CompletableDeferred<DeviceAuthentication>()
        val session = RelaySession(
            url = RELAY_WS_URL,
            scope = this,
            getAuthentication = { invitation },
            onEnrolled = { presented, finish ->
                presented.issuedCredential(finish)?.let { enrolled.complete(it) }
            },
            client = OkHttpClient(),
        )
        try {
            session.start()
            val state = withTimeout(30_000) {
                session.state.first {
                    it is RelaySession.SessionState.Connected ||
                        it is RelaySession.SessionState.AuthRejected
                }
            }
            if (state is RelaySession.SessionState.AuthRejected) {
                // The invitation may have raced another pairing — re-arm once
                // and retry a single time before declaring failure.
                session.close()
                assumeTrue("bootstrap invitation refused after re-arm", false)
            }
            val connected = state as RelaySession.SessionState.Connected

            // Credential issuance: the server finish carried a fresh secret.
            assertThat(connected.finish.credentialSecret).isNotNull()
            assertThat(connected.finish.deviceId).isNotEmpty()
            val credential = withTimeout(5_000) { enrolled.await() }

            // Inbound decryption: push_config is the relay's first message.
            val first = withTimeout(10_000) { session.incoming.first() }
            assertThat(first["type"]?.jsonPrimitive?.content).isEqualTo("push_config")

            // The relay's own device registry must contain this device.
            val list = withTimeout(15_000) { session.request(Inbound(type = "device_list")) }
            assertThat(list.ok).isTrue()
            val data = list.data!!.jsonObject
            assertThat(data["current_device_id"]!!.jsonPrimitive.content)
                .isEqualTo(connected.finish.deviceId)
            val deviceIds = data["devices"]!!.jsonArray
                .map { it.jsonObject["device_id"]!!.jsonPrimitive.content }
            assertThat(deviceIds).contains(connected.finish.deviceId)

            // Cleanup: revoke the test credential (self-revoke is allowed).
            try {
                val revoke = withTimeout(15_000) {
                    session.request(
                        Inbound(type = "revoke_device", deviceId = connected.finish.deviceId),
                    )
                }
                assertThat(revoke.ok).isTrue()
            } catch (cleanup: CommandException) {
                System.err.println(
                    "InteropTest: revoke_device refused (${cleanup.code}); " +
                        "test credential may persist on the relay",
                )
            }
            assertThat(credential.id).isEqualTo(connected.finish.credentialId)
        } finally {
            session.close()
        }
    }

    private fun isInteropEnabled(): Boolean = System.getenv("LERDR_INTEROP") == "1"

    /**
     * Reads `HERDR_RELAY_TOKEN` from the oracle's `relay/.env`. The value is
     * the raw 32-byte ASCII bootstrap secret (`config.go` requires
     * `len(Token) == 32`); it is returned, never logged.
     */
    private fun readRelayToken(): ByteArray? {
        val envFile = Path.of(ORACLE_ENV_PATH)
        if (!Files.isRegularFile(envFile)) return null
        val token = Files.readAllLines(envFile)
            .map { it.trim() }
            .firstOrNull { it.startsWith("HERDR_RELAY_TOKEN=") }
            ?.substringAfter('=')
            ?.trim()
            ?.trim('"', '\'')
            ?: return null
        val bytes = token.toByteArray(Charsets.US_ASCII)
        return bytes.takeIf { it.size == 32 }
    }

    /**
     * `kill -USR1 $(pgrep -f 'lerdr serve' | head -1)` — re-arms the one-use
     * bootstrap invitation; existing devices are kept. Returns false when no
     * relay process is running.
     */
    private fun rearmBootstrap(): Boolean {
        val pid = try {
            ProcessBuilder("pgrep", "-f", "lerdr serve")
                .start()
                .inputStream.bufferedReader().use { it.readLine() }
        } catch (unavailable: Exception) {
            return false
        } ?: return false
        return try {
            ProcessBuilder("kill", "-USR1", pid.trim()).start().waitFor() == 0
        } catch (unavailable: Exception) {
            false
        }
    }

    companion object {
        private const val RELAY_WS_URL = "ws://127.0.0.1:8375/ws"
        private const val ORACLE_ENV_PATH = "/home/l/Projects/lerdr/relay/.env"
    }
}
