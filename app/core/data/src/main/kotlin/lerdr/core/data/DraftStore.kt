package lerdr.core.data

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.MutablePreferences
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import java.io.IOException
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.Json

/**
 * Per-pane composer drafts — Lerdr's the local implementation persistence
 * tier on DataStore preferences instead of localStorage.
 *
 * Policy parity with Lerdr:
 * - TTL [MAX_AGE_MS] = 48 h — expired drafts decode as absent and are
 *   evicted by [prune] (or on read via [current]).
 * - [MAX_BYTES] = 64 KiB UTF-8 per draft — oversize text clears the stored
 *   record and reports [DraftSaveResult.TOO_LARGE], so an earlier shorter
 *   draft is not served back as current.
 * - [MAX_ENTRIES] = 64 drafts — beyond that the oldest `updatedAt` go.
 * - Empty text clears; saves are atomic per edit; IO failures map to
 *   [DraftSaveResult.UNAVAILABLE] (`'unavailable'` parity).
 *
 * ViewModels own active editor state and restore it when the draft identity
 * changes. Mutations are serialized in acceptance order; callers queuing
 * application-lifetime saves must enter [save] before yielding.
 */
class DraftStore(
    private val dataStore: DataStore<Preferences>,
    private val now: () -> Long = System::currentTimeMillis,
    private val json: Json = Json,
) {
    private val mutations = Mutex()

    /**
     * The live draft for [identity], or null — expired and malformed
     * records decode as absent. Cold per-collector; use [current] for a
     * one-shot read.
     */
    fun draft(identity: String): Flow<ComposerDraft?> = dataStore.data
        .map { prefs -> decode(prefs[draftKey(identity)], identity) }
        .distinctUntilChanged()

    /** One-shot read; an expired record is physically evicted like Lerdr's `loadPromptDraft`. */
    suspend fun current(identity: String): ComposerDraft? {
        val key = draftKey(identity)
        val raw = dataStore.data.first()[key] ?: return null
        val draft = decode(raw, identity)
        if (draft == null) {
            // Stored but unparseable/foreign/expired — remove on read.
            mutations.withLock {
                dataStore.edit { prefs ->
                    if (prefs[key] == raw) prefs.remove(key)
                }
            }
        }
        return draft
    }

    /**
     * `savePromptDraft` — non-empty within [MAX_BYTES] writes and prunes;
     * empty clears; oversize clears and reports TOO_LARGE.
     */
    suspend fun save(identity: String, text: String): DraftSaveResult = mutations.withLock {
        saveLocked(identity, text)
    }

    private suspend fun saveLocked(identity: String, text: String): DraftSaveResult = try {
        val key = draftKey(identity)
        if (text.isEmpty()) {
            dataStore.edit { it.remove(key) }
            return DraftSaveResult.CLEARED
        }
        if (text.toByteArray(Charsets.UTF_8).size > MAX_BYTES) {
            dataStore.edit { it.remove(key) }
            return DraftSaveResult.TOO_LARGE
        }
        val at = now()
        dataStore.edit { prefs ->
            prefs[key] = json.encodeToString(
                DraftRecord.serializer(),
                DraftRecord(DRAFT_VERSION, identity, text, at),
            )
            pruneLocked(prefs, at)
        }
        DraftSaveResult.SAVED
    } catch (e: IOException) {
        DraftSaveResult.UNAVAILABLE
    }

    /** `clearPromptDraft`. */
    suspend fun clear(identity: String) = mutations.withLock {
        dataStore.edit { it.remove(draftKey(identity)) }
        Unit
    }

    /** Clear a submitted snapshot without removing a newer draft for the same identity. */
    suspend fun clearIfMatches(identity: String, submittedText: String) = mutations.withLock {
        val key = draftKey(identity)
        dataStore.edit { prefs ->
            if (decode(prefs[key], identity)?.text == submittedText) prefs.remove(key)
        }
        Unit
    }

    /**
     * `prunePromptDrafts` — drops unparseable and expired records, then
     * evicts oldest-first beyond [MAX_ENTRIES].
     * @return the storage keys that were removed.
     */
    suspend fun prune(): List<String> = mutations.withLock {
        val removed = mutableListOf<String>()
        dataStore.edit { prefs -> removed += pruneLocked(prefs, now()) }
        removed
    }

    private fun pruneLocked(prefs: MutablePreferences, at: Long): List<String> {
        val live = mutableListOf<Pair<String, Long>>()
        val removed = mutableListOf<String>()
        for (key in prefs.asMap().keys) {
            if (!key.name.startsWith(DRAFT_PREFIX)) continue
            val record = decode(prefs[stringPreferencesKey(key.name)], null)
            if (record == null) {
                prefs.remove(stringPreferencesKey(key.name))
                removed += key.name
            } else {
                live += key.name to record.updatedAtEpochMs
            }
        }
        live.sortByDescending { it.second }
        for ((name, _) in live.drop(MAX_ENTRIES)) {
            prefs.remove(stringPreferencesKey(name))
            removed += name
        }
        return removed
    }

    /** Validates version, identity match, freshness — `parseDraft` + expiry. */
    private fun decode(raw: String?, identity: String?): ComposerDraft? {
        if (raw == null) return null
        val record = try {
            json.decodeFromString(DraftRecord.serializer(), raw)
        } catch (e: Exception) {
            return null
        }
        if (record.version != DRAFT_VERSION || record.updatedAtEpochMs <= 0) return null
        if (identity != null && record.identity != identity) return null
        if (now() - record.updatedAtEpochMs > MAX_AGE_MS) return null
        return ComposerDraft(record.identity, record.text, record.updatedAtEpochMs)
    }

    companion object {
        /** Lerdr key prefix. */
        const val DRAFT_PREFIX = "lerdr_prompt_draft_v1:"
        const val DRAFT_VERSION = 1
        const val MAX_AGE_MS = 48L * 60 * 60 * 1_000
        const val MAX_BYTES = 64 * 1_024
        const val MAX_ENTRIES = 64

        fun draftKey(identity: String): Preferences.Key<String> =
            stringPreferencesKey("$DRAFT_PREFIX$identity")
    }
}
