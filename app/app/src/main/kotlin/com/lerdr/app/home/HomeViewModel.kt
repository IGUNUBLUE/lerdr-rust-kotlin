package com.lerdr.app.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.stateIn

/**
 * Plain ViewModel — `hilt-navigation-compose` is not on the classpath, so
 * constructor-injected `@HiltViewModel`s can't resolve inside Nav3 entry
 * scopes. The repository is supplied through a `viewModel { }` initializer
 * (see [HomeScreen]); feature rounds swap in the `core:data` impl there.
 */
class HomeViewModel(
    repository: HomeRepository,
) : ViewModel() {

    val uiState: StateFlow<HomeUiState> = repository.uiState
        .stateIn(
            scope = viewModelScope,
            started = SharingStarted.WhileSubscribed(5_000),
            initialValue = HomeUiState(),
        )
}
