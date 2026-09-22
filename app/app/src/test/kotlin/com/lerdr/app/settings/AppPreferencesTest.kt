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

@OptIn(ExperimentalCoroutinesApi::class)
class AppPreferencesTest {

    @get:Rule
    val tmp = TemporaryFolder()

    @Test
    fun `theme mode defaults to SYSTEM and persists`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "app.preferences_pb")
        }
        val preferences = AppPreferences(dataStore)
        assertThat(preferences.themeMode.first()).isEqualTo(ThemeMode.SYSTEM)

        preferences.setThemeMode(ThemeMode.DARK)
        assertThat(preferences.themeMode.first()).isEqualTo(ThemeMode.DARK)

        // A fresh wrapper over the same store reads the durable value.
        assertThat(AppPreferences(dataStore).themeMode.first()).isEqualTo(ThemeMode.DARK)
    }

    @Test
    fun `unrecognized stored value falls back to SYSTEM`() = runTest {
        val dataStore = PreferenceDataStoreFactory.create(scope = backgroundScope) {
            File(tmp.root, "foreign.preferences_pb")
        }
        dataStore.edit { it[AppPreferences.THEME_MODE_KEY] = "midnight-solarized" }
        assertThat(AppPreferences(dataStore).themeMode.first()).isEqualTo(ThemeMode.SYSTEM)
    }
}
