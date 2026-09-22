package com.lerdr.app.security

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import javax.inject.Singleton

/** App-lock bindings — the testable seam over `androidx.biometric`. */
@Module
@InstallIn(SingletonComponent::class)
abstract class SecurityModule {

    @Binds
    @Singleton
    abstract fun bindBiometricPromptHelper(
        impl: AndroidBiometricPromptHelper,
    ): BiometricPromptHelper
}
