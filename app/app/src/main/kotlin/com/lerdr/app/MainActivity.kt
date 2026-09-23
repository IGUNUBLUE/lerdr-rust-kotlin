package com.lerdr.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.compose.setContent
import androidx.fragment.app.FragmentActivity
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.navigation3.runtime.EntryProviderScope
import com.lerdr.app.activity.ActivityScreen
import com.lerdr.app.computers.ComputersScreen
import com.lerdr.app.home.HomeScreen
import com.lerdr.app.notify.RequestPostNotificationsPermission
import com.lerdr.app.pairing.PairingScreen
import com.lerdr.app.security.LockGate
import com.lerdr.app.session.AgentFeedScreen
import com.lerdr.app.session.FilesScreen
import com.lerdr.app.session.SessionRepository
import com.lerdr.app.session.TerminalScreen
import com.lerdr.app.settings.SettingsScreen
import com.lerdr.core.designsystem.theme.LerdrTheme
import com.lerdr.navigation.LerdrDeepLinks
import com.lerdr.navigation.LerdrKey
import com.lerdr.navigation.LerdrNavDisplay
import com.lerdr.navigation.LerdrNavigator
import com.lerdr.navigation.rememberLerdrNavigator
import dagger.hilt.android.AndroidEntryPoint
import javax.inject.Inject
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.receiveAsFlow

@AndroidEntryPoint
class MainActivity : FragmentActivity() {

    @Inject
    lateinit var sessions: SessionRepository

    /**
     * Deep links that arrive while the app is running (singleTop) — emitted
     * into composition where the navigator consumes them.
     */
    private val deepLinks = Channel<String>(Channel.BUFFERED)

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        // Cold-start deep link seeds the back stack under Home so Back
        // always lands on mission control ("exit through home").
        val deepLinkedKey = intent?.dataString?.let(LerdrDeepLinks::match)

        setContent {
            LerdrTheme {
                RequestPostNotificationsPermission()

                val navigator = rememberLerdrNavigator(
                    *remember {
                        buildList {
                            add(LerdrKey.Home)
                            deepLinkedKey?.let(::add)
                        }.toTypedArray()
                    },
                )

                LaunchedEffect(Unit) {
                    deepLinks.receiveAsFlow().collect { link ->
                        LerdrDeepLinks.match(link)?.let(navigator::navigate)
                    }
                }

                // App-lock gate — the oracle "verifies before it will
                // connect at open"; locked content is never composed.
                LockGate {
                    LerdrNavDisplay(navigator = navigator) {
                        lerdrEntries(navigator)
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.dataString?.let { deepLinks.trySend(it) }
    }

    override fun onStart() {
        super.onStart()
        // Foreground — sessions resume keepalives; revalidate probes each
        // connection for staleness (the oracle's revalidateConnections).
        sessions.setHidden(false)
        sessions.revalidateAll()
    }

    override fun onStop() {
        super.onStop()
        sessions.setHidden(true)
    }
}

/**
 * The MVP graph — feature modules will later contribute their own
 * `EntryProviderScope<LerdrKey>.section()` blocks (nav3 modular pattern);
 * for the shell everything lives here.
 */
private fun EntryProviderScope<LerdrKey>.lerdrEntries(
    navigator: LerdrNavigator,
) {
    entry<LerdrKey.Home> {
        HomeScreen(
            onOpenAgent = navigator::openAgent,
            onSelectTopLevel = navigator::navigateTopLevel,
        )
    }
    entry<LerdrKey.Computers> {
        ComputersScreen(
            onSelectTopLevel = navigator::navigateTopLevel,
            onPairDevice = { navigator.openPairing() },
            onManageDevices = { navigator.navigateTopLevel(LerdrKey.Settings) },
        )
    }
    entry<LerdrKey.Activity> {
        ActivityScreen(onSelectTopLevel = navigator::navigateTopLevel)
    }
    entry<LerdrKey.Settings> {
        SettingsScreen(onSelectTopLevel = navigator::navigateTopLevel)
    }
    entry<LerdrKey.Pairing> { key ->
        PairingScreen(
            setupLink = key.setupLink,
            onPaired = navigator::onPairingComplete,
            onBack = navigator::goBack,
        )
    }
    entry<LerdrKey.AgentFeed> { key ->
        AgentFeedScreen(
            paneId = key.paneId,
            onOpenTerminal = { navigator.openTerminal(key.paneId) },
            onOpenFiles = { navigator.openFiles(key.paneId) },
            onBack = navigator::goBack,
            onSelectTab = { navigator.openAgent(it) },
        )
    }
    entry<LerdrKey.Terminal> { key ->
        TerminalScreen(
            paneId = key.paneId,
            onOpenFeed = { navigator.openAgent(key.paneId) },
            onOpenFiles = { navigator.openFiles(key.paneId) },
            onBack = navigator::goBack,
            onSelectTab = { navigator.openTerminal(it) },
        )
    }
    entry<LerdrKey.Files> { key ->
        FilesScreen(
            paneId = key.paneId,
            onOpenFeed = { navigator.openAgent(key.paneId) },
            onOpenTerminal = { navigator.openTerminal(key.paneId) },
            onBack = navigator::goBack,
            onSelectTab = { navigator.openFiles(it) },
        )
    }
}
