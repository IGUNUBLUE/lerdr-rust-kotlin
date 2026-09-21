package com.lerdr.app.di

import javax.inject.Qualifier

/**
 * The application-lifetime coroutine scope — stores that must outlive any
 * screen (relay sessions, credential persistence, draft writes) hang off
 * this, never `viewModelScope`/`lifecycleScope`.
 */
@Qualifier
@Retention(AnnotationRetention.BINARY)
annotation class AppScope
