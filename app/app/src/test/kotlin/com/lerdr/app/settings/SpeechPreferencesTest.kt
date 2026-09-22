package com.lerdr.app.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import com.google.common.truth.Truth.assertThat
import java.io.File
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * `lerdr_speech_enabled` / `lerdr_speech_language` — the oracle's
 * localStorage keys on DataStore, including the `adoptRelaySpeech`
 * first-relay onboarding.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class SpeechPreferencesTest {

    @get:Rule
    val tmp = TemporaryFolder()

    @Test
    fun `defaults are off and english`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "defaults.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        assertThat(preferences.enabled.first()).isFalse()
        assertThat(preferences.language.first()).isEqualTo("en")
    }

    @Test
    fun `enabled persists`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "enabled.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.setEnabled(true)
        assertThat(preferences.enabled.first()).isTrue()
        assertThat(SpeechPreferences(dataStore).enabled.first()).isTrue()
    }

    @Test
    fun `language persists a speakable code`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "language.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.setLanguage("fr")
        assertThat(preferences.language.first()).isEqualTo("fr")
        assertThat(SpeechPreferences(dataStore).language.first()).isEqualTo("fr")
    }

    @Test
    fun `unspeakable codes are refused`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "refused.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        preferences.setLanguage("ja")
        assertThat(preferences.language.first()).isEqualTo("en")
    }

    @Test
    fun `device language seeds the default when speakable`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "device.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "de" }
        assertThat(preferences.language.first()).isEqualTo("de")
    }

    @Test
    fun `unspeakable device language falls back to english`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "fallback.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "ja" }
        assertThat(preferences.language.first()).isEqualTo("en")
    }

    @Test
    fun `an invalid stored code falls back like an unset one`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "invalid.preferences_pb")
        }
        dataStore.edit { it[SpeechPreferences.LANGUAGE_KEY] = "xx" }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        assertThat(preferences.language.first()).isEqualTo("en")
    }

    // ── adoptRelaySpeech ────────────────────────────────────────────────

    @Test
    fun `adopt enables speech and picks a speakable language`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        preferences.adoptRelaySpeech(listOf("en", "fr"))
        assertThat(preferences.enabled.first()).isTrue()
        assertThat(preferences.language.first()).isEqualTo("en")
    }

    @Test
    fun `adopt picks the first relay language when english is absent`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt-first.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        preferences.adoptRelaySpeech(listOf("fr", "de"))
        assertThat(preferences.language.first()).isEqualTo("fr")
        assertThat(preferences.enabled.first()).isTrue()
    }

    @Test
    fun `adopt prefers the device language when the relay speaks it`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt-device.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "es" }
        preferences.adoptRelaySpeech(listOf("en", "es"))
        assertThat(preferences.language.first()).isEqualTo("es")
    }

    @Test
    fun `adopt never overrides a stored choice`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt-kept.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        preferences.setEnabled(false)
        preferences.setLanguage("de")
        preferences.adoptRelaySpeech(listOf("en"))
        assertThat(preferences.enabled.first()).isFalse()
        assertThat(preferences.language.first()).isEqualTo("de")
    }

    @Test
    fun `adopt fills only the unset key`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt-partial.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        // User already touched the toggle — adoption must not re-enable.
        preferences.setEnabled(false)
        preferences.adoptRelaySpeech(listOf("zh"))
        assertThat(preferences.enabled.first()).isFalse()
        assertThat(preferences.language.first()).isEqualTo("zh")
    }

    @Test
    fun `adopt with no speakable language is a no-op`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "adopt-empty.preferences_pb")
        }
        val preferences = SpeechPreferences(dataStore)
        preferences.deviceLanguage = { "en" }
        preferences.adoptRelaySpeech(listOf("ja", "ko"))
        preferences.adoptRelaySpeech(emptyList())
        assertThat(preferences.enabled.first()).isFalse()
        assertThat(preferences.language.first()).isEqualTo("en")
    }
}
