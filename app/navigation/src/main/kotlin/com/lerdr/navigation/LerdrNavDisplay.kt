package com.lerdr.navigation

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.EntryProviderScope
import androidx.navigation3.runtime.NavEntry
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay

/**
 * The app's [NavDisplay] with Lerdr defaults wired:
 * - `onBack` pops the typed back stack (predictive back comes free).
 * - `entryDecorators` adds per-entry `ViewModelStore` scoping
 *   ([rememberViewModelStoreNavEntryDecorator]) on top of the saveable-state
 *   holder — entries get ViewModels keyed by nav key.
 *
 * Feature screens contribute destinations through [entryBuilder] — the
 * `EntryProviderScope<LerdrKey>` receiver keeps everything on the sealed key
 * type.
 */
@Composable
fun LerdrNavDisplay(
    navigator: LerdrNavigator,
    modifier: Modifier = Modifier,
    entryBuilder: EntryProviderScope<LerdrKey>.() -> Unit,
) {
    NavDisplay(
        backStack = navigator.backStack,
        modifier = modifier,
        onBack = navigator::goBack,
        entryDecorators = listOf(
            rememberSaveableStateHolderNavEntryDecorator(),
            rememberViewModelStoreNavEntryDecorator(),
        ),
        entryProvider = entryProvider<LerdrKey>(
            fallback = { key ->
                NavEntry(key) {
                    error("No navigation entry registered for $key")
                }
            },
            builder = entryBuilder,
        ),
    )
}
