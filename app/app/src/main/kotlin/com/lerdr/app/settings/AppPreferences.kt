package com.lerdr.app.settings

import androidx.compose.runtime.Immutable
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import javax.inject.Inject
import javax.inject.Singleton
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/**
 * App-level display preferences — non-secret UI state on the shared
 * `lerdr` preferences DataStore (the same instance [RelayRegistry] and
 * `DraftStore` use; keys are namespaced).
 *
 * [ThemeMode] is the source of truth for the Settings theme picker; the
 * visual application is a separate platform concern — on API 31+ the
 * screen also calls `UiModeManager.setApplicationNightMode`, which is the
 * mechanism that actually flips `isSystemInDarkTheme` for the app
 * (no appcompat on the classpath).
 */
@Singleton
class AppPreferences @Inject constructor(
    private val dataStore: DataStore<Preferences>,
) {

    /** Persisted theme selection — defaults to [ThemeMode.SYSTEM]. */
    val themeMode: Flow<ThemeMode> = dataStore.data
        .map { ThemeMode.parse(it[THEME_MODE_KEY]) }
        .distinctUntilChanged()

    suspend fun setThemeMode(mode: ThemeMode) {
        dataStore.edit { it[THEME_MODE_KEY] = mode.stored }
    }

    /**
     * Opt-in app lock (oracle docs/security.md "device verification") —
     * when on, `security.LockGate` covers the UI at process start until
     * `BiometricPrompt` verifies the user once. Defaults off. UX gate
     * only: this flag is not a secret and the Keystore credential seal
     * is unaffected by it.
     */
    val appLockEnabled: Flow<Boolean> = dataStore.data
        .map { it[APP_LOCK_ENABLED_KEY] ?: false }
        .distinctUntilChanged()

    suspend fun setAppLockEnabled(enabled: Boolean) {
        dataStore.edit { it[APP_LOCK_ENABLED_KEY] = enabled }
    }

    companion object {
        val THEME_MODE_KEY = stringPreferencesKey("lerdr_theme_mode")
        val APP_LOCK_ENABLED_KEY = booleanPreferencesKey("lerdr_app_lock_enabled")
    }
}

/** Theme picker choices — `stored` is the durable wire value. */
@Immutable
enum class ThemeMode(val stored: String) {
    SYSTEM("system"),
    LIGHT("light"),
    DARK("dark"),
    ;

    companion object {
        /** Unknown/absent values fall back to following the system. */
        fun parse(raw: String?): ThemeMode =
            entries.firstOrNull { it.stored == raw } ?: SYSTEM
    }
}
