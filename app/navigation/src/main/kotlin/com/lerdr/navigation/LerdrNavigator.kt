package com.lerdr.navigation

import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSerializable
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.serialization.NavBackStackSerializer
import androidx.navigation3.runtime.serialization.NavKeySerializer

/**
 * Typed back stack that survives config changes and process death —
 * `rememberNavBackStack` overload that keeps the [LerdrKey] element type
 * (pattern from the nav3 conditional-navigation recipe).
 */
@Composable
fun rememberLerdrBackStack(vararg elements: LerdrKey): NavBackStack<LerdrKey> =
    rememberSerializable(
        serializer = NavBackStackSerializer(elementSerializer = NavKeySerializer()),
    ) {
        NavBackStack(*elements)
    }

/**
 * Navigator + typed back stack in one call — the app's navigation root.
 */
@Composable
fun rememberLerdrNavigator(vararg elements: LerdrKey): LerdrNavigator {
    val backStack = rememberLerdrBackStack(*elements)
    return remember(backStack) { LerdrNavigator(backStack) }
}

/**
 * Single back-stack navigator for the MVP graph.
 *
 * Semantics:
 * - Top-level keys ([LerdrKey.topLevel]) swap the current tab rather than
 *   stack — reselecting a tab never duplicates it.
 * - [LerdrKey.AgentFeed], [LerdrKey.Terminal] and [LerdrKey.Files] are *modes
 *   of one session*: switching modes for the same pane replaces the top
 *   entry instead of stacking feed↔terminal↔files onto the back stack.
 * - Pairing is a modal overlay; completing it drops the key wherever it sat.
 */
class LerdrNavigator(val backStack: NavBackStack<LerdrKey>) {

    fun navigate(key: LerdrKey) {
        backStack.add(key)
    }

    fun goBack() {
        backStack.removeLastOrNull()
    }

    /** Bottom-bar navigation — tabs replace, Home pops to the root. */
    fun navigateTopLevel(key: LerdrKey) {
        when (key) {
            LerdrKey.Home -> {
                while (backStack.size > 1 && backStack.last() != LerdrKey.Home) {
                    backStack.removeLastOrNull()
                }
            }
            else -> {
                if (backStack.lastOrNull() == key) return
                backStack.removeAll { it == key }
                backStack.add(key)
            }
        }
    }

    fun openPairing(setupLink: SetupLink? = null) {
        navigate(LerdrKey.Pairing(setupLink))
    }

    /** Feed is the default agent-session mode. */
    fun openAgent(paneId: String) {
        navigateAgentMode(LerdrKey.AgentFeed(paneId))
    }

    fun openTerminal(paneId: String) {
        navigateAgentMode(LerdrKey.Terminal(paneId))
    }

    fun openFiles(paneId: String) {
        navigateAgentMode(LerdrKey.Files(paneId))
    }

    /** Mode switch: replace in place when the pane matches, else push. */
    private fun navigateAgentMode(key: LerdrKey) {
        val paneId = when (key) {
            is LerdrKey.AgentFeed -> key.paneId
            is LerdrKey.Terminal -> key.paneId
            is LerdrKey.Files -> key.paneId
            else -> return
        }
        val topPaneId = when (val top = backStack.lastOrNull()) {
            is LerdrKey.AgentFeed -> top.paneId
            is LerdrKey.Terminal -> top.paneId
            is LerdrKey.Files -> top.paneId
            else -> null
        }
        if (topPaneId == paneId) swapTop(key) else backStack.add(key)
    }

    private fun swapTop(key: LerdrKey) {
        if (backStack.isNotEmpty()) backStack[backStack.lastIndex] = key
    }

    /** Pairing succeeded — drop all pairing entries, land on Home. */
    fun onPairingComplete() {
        backStack.removeAll { it is LerdrKey.Pairing }
        if (backStack.isEmpty() || backStack.first() != LerdrKey.Home) {
            backStack.add(0, LerdrKey.Home)
        }
    }
}
