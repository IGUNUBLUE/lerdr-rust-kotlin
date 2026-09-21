package com.lerdr.app.di

import com.lerdr.app.home.FakeHomeRepository
import com.lerdr.app.home.HomeRepository
import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent
import javax.inject.Singleton

/**
 * App-level bindings. Feature rounds replace [FakeHomeRepository] with the
 * `core:data`-backed implementation — screens only see [HomeRepository].
 */
@Module
@InstallIn(SingletonComponent::class)
abstract class AppModule {

    @Binds
    @Singleton
    abstract fun bindHomeRepository(impl: FakeHomeRepository): HomeRepository
}
