package lerdr.core.data

import android.content.Context
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.AtomicMoveNotSupportedException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.intOrNull

/**
 * [CredentialStore] over a Keystore-sealed file — the production
 * counterpart of the oracle's plaintext `lerdr_device_auth_v1` blob.
 *
 * Disk format: `MAGIC(4) ‖ format_version(1) ‖ cipher.seal(json)` where
 * `json` is `{version:1, relays:{<relayId>: <record>}}` — the oracle's
 * `PersistedDeviceAuthState` shape. Secrets exist only inside the sealed
 * region; nothing here logs or stores them unwrapped.
 *
 * Persistence discipline mirrors `pairing-store.md §B.4`: write tmp →
 * fsync → atomic rename → fsync dir. A failed persist leaves the previous
 * file and the in-memory map untouched (read-modify-write-persist, rollback
 * on persist failure).
 */
class KeystoreCredentialStore(
    private val file: File,
    private val cipher: CredentialCipher,
    private val scope: CoroutineScope,
    private val now: () -> Long = System::currentTimeMillis,
) : CredentialStore {

    private val mutex = Mutex()
    private var loaded = false

    private val _records = MutableStateFlow<Map<String, RelayDeviceAuth>>(emptyMap())
    override val records: StateFlow<Map<String, RelayDeviceAuth>> = _records.asStateFlow()

    init {
        // Warm the flow eagerly so collectors see stored records without
        // needing a first mutation; ops still lazily ensureLoaded().
        scope.launch { mutex.withLock { ensureLoadedLocked() } }
    }

    override suspend fun get(relayId: String): RelayDeviceAuth? {
        val id = requireIdentifier(relayId, "relay id")
        return mutex.withLock {
            ensureLoadedLocked()
            val entry = _records.value[id]
            if (entry is RelayInvitation && entry.isExpired(now())) {
                // Expired invitations self-evict on read (device-auth.ts:91-94).
                persistLocked(_records.value - id)
                null
            } else {
                entry
            }
        }
    }

    override suspend fun saveInvitation(
        relayId: String,
        invitation: RelayInvitation,
    ): RelayInvitation {
        val id = requireIdentifier(relayId, "relay id")
        require(!invitation.isExpired(now())) { "The device invitation has expired." }
        return mutex.withLock {
            ensureLoadedLocked()
            persistLocked(_records.value + (id to invitation))
            invitation
        }
    }

    override suspend fun redeemInvitation(
        relayId: String,
        invitationId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential {
        val id = requireIdentifier(relayId, "relay id")
        val expected = requireIdentifier(invitationId, "invitation id")
        val secret = enrollment.credentialSecret
            ?: throw IllegalArgumentException("Relay did not issue a device credential.")
        return mutex.withLock {
            ensureLoadedLocked()
            val current = _records.value[id]
            check(current is RelayInvitation && current.id == expected) {
                "The redeemed device invitation is no longer available."
            }
            val credential = enrollment.toCredential(secret, expected)
            persistLocked(_records.value + (id to credential))
            credential
        }
    }

    override suspend fun updateCredential(
        relayId: String,
        enrollment: CredentialEnrollment,
    ): RelayDeviceCredential {
        val id = requireIdentifier(relayId, "relay id")
        require(enrollment.credentialSecret == null) {
            "Relay unexpectedly replaced an enrolled device credential."
        }
        return mutex.withLock {
            ensureLoadedLocked()
            val current = _records.value[id] as? RelayDeviceCredential
                ?: throw IllegalStateException("No device credential is stored for this relay.")
            check(current.id == enrollment.credentialId && current.deviceId == enrollment.deviceId) {
                "The relay authenticated a different device credential."
            }
            val next = current.copy(
                version = enrollment.credentialVersion,
                role = enrollment.role,
                locale = enrollment.locale,
            )
            persistLocked(_records.value + (id to next))
            next
        }
    }

    override suspend fun remove(relayId: String): Boolean {
        val id = requireIdentifier(relayId, "relay id")
        return mutex.withLock {
            ensureLoadedLocked()
            if (!_records.value.containsKey(id)) return@withLock false
            persistLocked(_records.value - id)
            true
        }
    }

    override suspend fun clear() {
        mutex.withLock {
            ensureLoadedLocked()
            if (_records.value.isNotEmpty()) persistLocked(emptyMap())
        }
    }

    // ── persistence ──────────────────────────────────────────────────

    /** Caller must hold [mutex]. */
    private suspend fun ensureLoadedLocked() {
        if (loaded) return
        loaded = true
        _records.value = withContext(Dispatchers.IO) { readFile() }
    }

    /** Caller must hold [mutex]; on success the flow publishes [next]. */
    private suspend fun persistLocked(next: Map<String, RelayDeviceAuth>) {
        val state = PersistedAuthState(relays = next)
        val plaintext = json.encodeToString(PersistedAuthState.serializer(), state)
            .toByteArray(Charsets.UTF_8)
        val sealed = try {
            cipher.seal(plaintext)
        } finally {
            plaintext.fill(0)
        }
        withContext(Dispatchers.IO) { writeFile(sealed) }
        _records.value = next
    }

    private fun readFile(): Map<String, RelayDeviceAuth> {
        val raw = try {
            if (!file.isFile || file.length() > MAX_FILE_BYTES) return emptyMap()
            file.readBytes()
        } catch (e: IOException) {
            return emptyMap()
        }
        if (raw.size <= ENVELOPE_HEADER_BYTES) return emptyMap()
        if (!raw.copyOfRange(0, MAGIC.size).contentEquals(MAGIC) ||
            raw[MAGIC.size] != FORMAT_VERSION.toByte()) {
            return emptyMap()
        }
        val plaintext = try {
            cipher.open(raw.copyOfRange(ENVELOPE_HEADER_BYTES, raw.size))
        } catch (e: Exception) {
            return emptyMap()
        }
        val root = try {
            json.parseToJsonElement(String(plaintext, Charsets.UTF_8)) as? JsonObject
        } catch (e: IllegalArgumentException) {
            null
        } finally {
            plaintext.fill(0)
        } ?: return emptyMap()
        if ((root["version"] as? JsonPrimitive)?.intOrNull != STATE_VERSION) return emptyMap()
        val relays = root["relays"] as? JsonObject ?: return emptyMap()
        // Per-entry leniency (device-auth.ts:170-180): one malformed record
        // never forfeits the rest.
        return relays.entries.mapNotNull { (relayId, element) ->
            try {
                val id = requireIdentifier(relayId, "relay id")
                id to json.decodeFromJsonElement(RelayDeviceAuth.serializer(), element)
            } catch (e: Exception) {
                null
            }
        }.toMap()
    }

    private fun writeFile(sealed: ByteArray) {
        val dir = file.parentFile ?: file.absoluteFile.parentFile
        dir?.mkdirs()
        val tmp = File(dir, "${file.name}.tmp")
        val payload = MAGIC + byteArrayOf(FORMAT_VERSION.toByte()) + sealed
        try {
            FileOutputStream(tmp).channel.use { channel ->
                channel.write(java.nio.ByteBuffer.wrap(payload))
                channel.force(true)
            }
            try {
                Files.move(
                    tmp.toPath(), file.toPath(),
                    StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING,
                )
            } catch (e: AtomicMoveNotSupportedException) {
                Files.move(tmp.toPath(), file.toPath(), StandardCopyOption.REPLACE_EXISTING)
            }
            dir?.let { directory ->
                try {
                    FileChannel.open(directory.toPath()).use { it.force(true) }
                } catch (e: IOException) {
                    // Directory sync is best-effort on Android filesystems.
                }
            }
        } finally {
            if (tmp.exists()) tmp.delete()
        }
    }

    @Serializable
    private data class PersistedAuthState(
        // Always emitted — readFile rejects a blob without it.
        @EncodeDefault(EncodeDefault.Mode.ALWAYS) val version: Int = STATE_VERSION,
        val relays: Map<String, RelayDeviceAuth> = emptyMap(),
    )

    private fun CredentialEnrollment.toCredential(
        secret: ByteArray,
        invitationId: String,
    ): RelayDeviceCredential = RelayDeviceCredential(
        id = credentialId,
        version = credentialVersion,
        secret = Base64Url.encode(secret),
        deviceId = deviceId,
        role = role,
        locale = locale,
        issuedAtEpochMs = now(),
        invitationId = invitationId,
    )

    companion object {
        private val MAGIC = byteArrayOf('L'.code.toByte(), 'D'.code.toByte(), 'K'.code.toByte(), 'C'.code.toByte())
        private const val FORMAT_VERSION = 1
        private const val STATE_VERSION = 1
        private val ENVELOPE_HEADER_BYTES get() = MAGIC.size + 1
        private const val MAX_FILE_BYTES = 1L * 1024 * 1024

        private val json = Json {
            ignoreUnknownKeys = true
            classDiscriminator = "kind"
        }

        /**
         * App-facing factory: sealed credentials under
         * `noBackupFilesDir/pairing/` — device-bound identity must not ride
         * cloud backups onto other hardware.
         */
        fun create(context: Context, scope: CoroutineScope): KeystoreCredentialStore =
            KeystoreCredentialStore(
                file = File(context.noBackupFilesDir, "pairing/credentials.dat"),
                cipher = AndroidKeystoreCipher(),
                scope = scope,
            )
    }
}
