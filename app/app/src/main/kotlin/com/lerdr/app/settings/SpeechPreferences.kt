package com.lerdr.app.settings

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import com.lerdr.app.speech.isSpeechLanguage
import java.util.Locale
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/**
 * Read-aloud preferences — the oracle's `lerdr_speech_enabled` /
 * `lerdr_speech_language` localStorage keys on the shared `lerdr`
 * preferences DataStore (namespaced beside [AppPreferences]' keys).
 *
 * Mirrors `frontend/src/lib/speech.ts`:
 * - [enabled] defaults off; [language] resolves a stored valid code, else
 *   the device language when it is speakable, else English.
 * - [setLanguage] refuses codes outside `SPEECH_LANGUAGES` — the oracle's
 *   `isSpeechLanguage` guard.
 * - [adoptRelaySpeech] is the one-time onboarding: the first relay that
 *   advertises speakable languages picks the language it can actually
 *   speak (when the user never chose one) and turns reading on (when the
 *   toggle was never touched). After that the settings are the user's and
 *   a relay never flips them back.
 */
@Singleton
class SpeechPreferences @Inject constructor(
    private val dataStore: DataStore<Preferences>,
) {
    /** Device-language seam — `navigator.language`; tests pin the locale. */
    internal var deviceLanguage: () -> String = { Locale.getDefault().language }

    /** `speechEnabled` — persisted toggle, default off. */
    val enabled: Flow<Boolean> = dataStore.data
        .map { it[ENABLED_KEY] ?: false }
        .distinctUntilChanged()

    /**
     * `speechLanguage` — effective selection: the stored code when it is a
     * speakable language, else the device language when speakable, else
     * English (`storedLanguage()` in the oracle).
     */
    val language: Flow<String> = dataStore.data
        .map { resolveLanguage(it[LANGUAGE_KEY]) }
        .distinctUntilChanged()

    /** `setSpeechEnabled` — persists the toggle; the player observes. */
    suspend fun setEnabled(enabled: Boolean) {
        dataStore.edit { it[ENABLED_KEY] = enabled }
    }

    /** `setSpeechLanguage` — persists a speakable code; others are refused. */
    suspend fun setLanguage(code: String) {
        if (!isSpeechLanguage(code)) return
        dataStore.edit { it[LANGUAGE_KEY] = code }
    }

    /**
     * `adoptRelaySpeech` — when a relay reports speakable languages: fill
     * the unset language with one the relay can speak (preferring the
     * current effective language, then English) and default the toggle on
     * if the user never set it. Both writes ride one `edit` so they land
     * atomically — the oracle's two localStorage writes, tightened.
     */
    suspend fun adoptRelaySpeech(languages: List<String>) {
        val speakable = languages.filter(::isSpeechLanguage)
        if (speakable.isEmpty()) return
        dataStore.edit { prefs ->
            if (prefs[LANGUAGE_KEY] == null) {
                val preferred = listOf(resolveLanguage(null), DEFAULT_LANGUAGE)
                    .firstOrNull { it in speakable }
                    ?: speakable.first()
                prefs[LANGUAGE_KEY] = preferred
            }
            if (prefs[ENABLED_KEY] == null) {
                prefs[ENABLED_KEY] = true
            }
        }
    }

    /** `storedLanguage` — stored code, else device prefix, else English. */
    private fun resolveLanguage(stored: String?): String =
        stored?.takeIf(::isSpeechLanguage)
            ?: deviceLanguage()
                .substringBefore('-')
                .lowercase()
                .takeIf(::isSpeechLanguage)
            ?: DEFAULT_LANGUAGE

    companion object {
        val ENABLED_KEY = booleanPreferencesKey("lerdr_speech_enabled")
        val LANGUAGE_KEY = stringPreferencesKey("lerdr_speech_language")
        const val DEFAULT_LANGUAGE = "en"
    }
}
