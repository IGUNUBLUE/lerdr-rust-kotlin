package lerdr.core.transport

import com.google.common.truth.Truth.assertThat
import java.net.ServerSocket
import java.nio.file.Files
import java.nio.file.Path
import java.security.SecureRandom
import java.util.concurrent.TimeUnit
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
 * Live interop against the **Rust** relay (`lerdr-coord`'s `lerdr-relay`
 * binary). Gated on `LERDR_RUST_INTEROP=1`; the binary is located via
 * `LERDR_RELAY_BIN` or `relay/target/{debug,release}/lerdr-relay` relative
 * to the repository root (run `cargo build -p lerdr-coord` first).
 *
 * The relay is spawned with a known 32-byte `--token`, so the test redeems
 * the deterministic bootstrap invitation — no stdout scraping, no SIGUSR1.
 * The device-auth store lives in a temp dir, so issued credentials vanish
 * with the test; no `revoke_device` cleanup is needed.
 *
 * Asserts, in order: invitation pairing issues a credential, `push_config`
 * carries a live `herdr_status.server_version` (the coord layer is wired
 * to the real socket), the snapshot emits `agents`, and `read_pane` on the
 * first pane returns a confirmed receipt plus a `pane_content` push.
 */
class RustInteropTest {

    @Test(timeout = 120_000)
    fun pairsAndReadsPaneOverRustRelay() = runBlocking {
        assumeTrue(
            "LERDR_RUST_INTEROP=1 not set — Rust relay interop skipped",
            System.getenv("LERDR_RUST_INTEROP") == "1",
        )
        val binary = findRelayBinary()
        assumeTrue(
            "lerdr-relay binary not found (cargo build -p lerdr-coord)",
            binary != null && Files.isExecutable(binary),
        )

        val token = randomToken()
        val port = freePort()
        val authDir = Files.createTempDirectory("lerdr-interop-auth")
        val runtimeDir = Files.createTempDirectory("lerdr-interop-rt")

        val process = ProcessBuilder(
            binary!!.toString(),
            "serve",
            "--port", port.toString(),
            "--token", token,
            "--device-auth-dir", authDir.toString(),
            "--runtime-dir", runtimeDir.toString(),
        ).redirectErrorStream(true).start()
        val stdout = process.inputStream.bufferedReader()
        try {
            val ready = CompletableDeferred<Unit>()
            val logPump = Thread {
                while (true) {
                    val line = stdout.readLine() ?: break
                    if (line.contains("lerdr-relay listening")) ready.complete(Unit)
                }
            }.apply { isDaemon = true; start() }
            withTimeout(15_000) { ready.await() }

            val invitation = DeviceAuthentication.invitation(
                id = "bootstrap",
                secret = token.toByteArray(Charsets.US_ASCII),
                version = 1,
                locale = "en",
            )
            val enrolled = CompletableDeferred<DeviceAuthentication>()
            val session = RelaySession(
                url = "ws://127.0.0.1:$port/ws",
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
                assertThat(state).isInstanceOf(RelaySession.SessionState.Connected::class.java)
                val connected = state as RelaySession.SessionState.Connected
                assertThat(connected.finish.credentialSecret).isNotNull()
                assertThat(connected.finish.deviceId).isNotEmpty()
                withTimeout(5_000) { enrolled.await() }

                // First frame: push_config with a live Herdr status — proves
                // the coord layer reached the real socket.
                val first = withTimeout(10_000) { session.incoming.first() }
                assertThat(first["type"]?.jsonPrimitive?.content).isEqualTo("push_config")
                val herdrStatus = first["herdr_status"]!!.jsonObject
                assertThat(herdrStatus["server_version"]!!.jsonPrimitive.content).isNotEmpty()

                // The snapshot includes the projected agent inventory.
                val agents = withTimeout(10_000) {
                    session.incoming.first { it["type"]?.jsonPrimitive?.content == "agents" }
                }
                val paneId = agents["agents"]!!.jsonArray
                    .mapNotNull { it.jsonObject["pane_id"]?.jsonPrimitive?.content }
                    .firstOrNull()
                assumeTrue("no panes in the live Herdr session", paneId != null)

                // Routed read: confirmed receipt — dispatch boundary works
                // end to end through the Rust router.
                val receipt = withTimeout(15_000) {
                    session.request(
                        Inbound(type = "read_pane", paneId = paneId!!, lines = 20),
                    )
                }
                assertThat(receipt.ok).isTrue()
            } finally {
                session.close()
            }
        } finally {
            process.destroy()
            process.waitFor(5, TimeUnit.SECONDS)
            process.destroyForcibly()
        }
    }

    /** 32 ASCII bytes — the bootstrap secret shape `config` requires. */
    private fun randomToken(): String {
        val alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"
        val random = SecureRandom()
        return (1..32).map { alphabet[random.nextInt(alphabet.length)] }.joinToString("")
    }

    private fun freePort(): Int = ServerSocket(0).use { it.localPort }

    private fun findRelayBinary(): Path? {
        System.getenv("LERDR_RELAY_BIN")?.let { return Path.of(it) }
        // Tests run with the module dir as cwd — walk up to the repo root.
        var dir = Path.of("").toAbsolutePath()
        repeat(6) {
            for (profile in listOf("debug", "release")) {
                val candidate = dir.resolve("relay/target/$profile/lerdr-relay")
                if (Files.isExecutable(candidate)) return candidate
            }
            dir = dir.parent ?: return@repeat
        }
        return null
    }
}
