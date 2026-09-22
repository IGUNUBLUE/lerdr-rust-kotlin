package com.lerdr.app.security

import com.lerdr.app.settings.AppPreferences
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

/**
 * Singleton seams [LockGate] pulls through `EntryPointAccessors` —
 * `hilt-navigation-compose` is absent, so the `viewModel { }` factory in
 * the gate resolves its dependencies here instead of a `@HiltViewModel`
 * graph (same convention as `SettingsEntryPoint`).
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
interface SecurityEntryPoint {
    fun appPreferences(): AppPreferences
    fun biometricPromptHelper(): BiometricPromptHelper
    fun lockState(): LockState
}
