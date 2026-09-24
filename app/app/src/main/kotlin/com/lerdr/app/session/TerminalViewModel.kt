package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.ui.terminal.TerminalCursorUi
import com.lerdr.app.ui.terminal.TerminalRowUi
import com.lerdr.app.ui.terminal.parseTerminalRows
import com.lerdr.app.ui.terminal.terminalCursor
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.PaneLinkActivatedResult
import lerdr.core.model.PaneSearchResult
import lerdr.core.store.Agent
import lerdr.core.store.RelayStatus
import lerdr.core.terminal.PaneSurface
import lerdr.core.transport.CommandException

/** Everything Terminal mode renders — pane snapshot + key/send actions. */
@Immutable
data class TerminalUiState(
    val paneId: String,
    val title: String = "",
    val breadcrumb: String = "",
    /** Normalized agent identity ("claude", "codex"…) — top-bar logo. */
    val provider: String? = null,
    /** "lease 92×42" or the agent status — the chip in the top bar. */
    val statusLabel: String = "",
    val connected: Boolean = false,
    /** True until the first pane frame commits. */
    val waitingForContent: Boolean = true,
    val lines: List<String> = emptyList(),
    /**
     * The committed frame as parsed render rows — what [TerminalSurface]
     * draws. The instance is reused across metadata-only commits (the
     * parse is keyed on content, not revision), so Compose skips
     * re-measuring rows the delta did not touch.
     */
    val rows: List<TerminalRowUi> = emptyList(),
    /** Write cursor — last row, one cell past its content. */
    val cursor: TerminalCursorUi? = null,
    val revision: Long = 0,
    val truncated: Boolean = false,
    val noEcho: Boolean = false,
    val noEchoPrompt: String? = null,
    val leaseColumns: Int = 0,
    val leaseRows: Int = 0,
    /**
     * The oracle's `readOnly` gate, inverted — mutating affordances
     * (input bar, key chips, secret field) enable only for an enrolled
     * CONTROLLER credential. Fail-closed like the oracle.
     */
    val canControl: Boolean = false,
    /**
     * Relay `secret_input` capability — the hidden-prompt answer path
     * (`send_secret`). Without it the bar stays in plain mode and the
     * prompt banner carries the oracle's too-old-relay hint instead.
     */
    val secretInputSupported: Boolean = false,
    /**
     * Relay `pane_links` capability — the server hit-test path
     * (`pane_link_resolve`/`pane_link_activate`) that reaches OSC8 links
     * whose URL never appears in the served text.
     */
    val paneLinksSupported: Boolean = false,
    /**
     * Relay `pane_search` capability — server-side find over the pane's
     * full scrollback; the find bar annotates its count beyond the
     * rendered buffer.
     */
    val paneSearchSupported: Boolean = false,
    /** Transient action failure — rendered as a snackbar/inline error. */
    val lastError: String? = null,
)

/**
 * Terminal-mode mutation point — owns the pane watch for this screen's
 * lifetime, forwards key/input actions, and negotiates the size lease when
 * the view reports its measured grid.
 */
class TerminalViewModel(
    private val paneId: String,
    private val sessions: SessionRepository,
    private val appScope: kotlinx.coroutines.CoroutineScope,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")
    private val lastError = MutableStateFlow<String?>(null)

    /**
     * The lease the relay applied (clamped to its bounds), renewed on the
     * oracle's 10 s cadence — the relay TTL is ~120 s and the renewal is
     * also what re-arms the lease after a reconnect dropped it. Volatile:
     * written on viewModelScope, read on appScope.
     */
    @Volatile
    private var leasedColumns = 0

    @Volatile
    private var leasedRows = 0

    // Parse cache keyed on the committed content — a metadata-only delta
    // bumps revision without touching `lines`, so the row list survives
    // unchanged and the renderer keeps its measured draw state.
    private var parsedContent: String? = null
    private var parsedFormat: String = ""
    private var parsedRows: List<TerminalRowUi> = emptyList()
    private var parsedCursor: TerminalCursorUi? = null

    val uiState: StateFlow<TerminalUiState> = combine(
        sessions.paneSnapshot(paneId),
        sessions.agent(paneId),
        sessions.connection(relayId),
        lastError,
    ) { snapshot, agent, connection, error ->
        val content = snapshot?.content
        val format = snapshot?.format.orEmpty()
        if (content != parsedContent || format != parsedFormat) {
            parsedContent = content
            parsedFormat = format
            parsedRows = if (snapshot == null) {
                emptyList()
            } else {
                parseTerminalRows(snapshot.lines, format)
            }
            parsedCursor = terminalCursor(parsedRows)
        }
        TerminalUiState(
            paneId = paneId,
            title = agent?.name ?: agent?.agent ?: paneId.substringAfter("::"),
            provider = agent?.agent?.takeIf { it.isNotEmpty() },
            breadcrumb = breadcrumbOf(agent),
            statusLabel = if (snapshot != null && snapshot.columns > 0) {
                if (snapshot.rows > 0) {
                    "lease ${snapshot.columns}×${snapshot.rows}"
                } else {
                    "lease ${snapshot.columns} cols"
                }
            } else {
                agent?.status ?: ""
            },
            connected = connection?.status == RelayStatus.CONNECTED,
            waitingForContent = snapshot == null,
            lines = snapshot?.lines.orEmpty(),
            rows = parsedRows,
            cursor = parsedCursor,
            revision = snapshot?.revision ?: 0,
            truncated = snapshot?.truncated == true,
            noEcho = snapshot?.noEcho == true,
            noEchoPrompt = snapshot?.noEchoPrompt,
            leaseColumns = snapshot?.columns ?: 0,
            leaseRows = snapshot?.rows ?: 0,
            canControl = sessions.canControl(relayId),
            secretInputSupported = connection?.capabilities
                ?.contains(SessionRepository.SECRET_CAPABILITY) == true,
            paneLinksSupported = connection?.capabilities
                ?.contains(ClientCapabilities.PANE_LINKS) == true,
            paneSearchSupported = connection?.capabilities
                ?.contains(ClientCapabilities.PANE_SEARCH) == true,
            lastError = error,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), TerminalUiState(paneId))

    /**
     * Renewal + resume-edge jobs ride [appScope], not viewModelScope: a
     * repeating delay on Dispatchers.Main never lets `runTest`'s scheduler
     * go idle, while the test's backgroundScope is exempt. onCleared cancels
     * them explicitly since appScope outlives the ViewModel.
     */
    private var leaseLoopJob: Job? = null
    private var reLeaseJob: Job? = null

    init {
        // push_viewed_pane: the terminal view is the oracle's "viewed"
        // signal — entering publishes it, leaving clears it.
        sessions.setViewedPane(paneId)
        viewModelScope.launch { sessions.openPane(paneId) }
        leaseLoopJob = appScope.launch {
            while (true) {
                delay(LEASE_REFRESH_MS)
                // The oracle gates hidden renewals on a 5 min grace — after it
                // the relay TTL hands the pane's size back to the desktop.
                val columns = leasedColumns
                if (columns > 0 && sessions.paneLeaseRenewalAllowed()) {
                    try {
                        sessions.leasePaneSize(paneId, columns, leasedRows)
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (_: Exception) {
                        // Transient (disconnected, agent gone) — next tick retries.
                    }
                }
            }
        }
        reLeaseJob = appScope.launch {
            // Refocus parity: the moment the app is visible again the lease
            // re-arms instead of waiting out the renewal interval.
            sessions.hidden.collect { hidden ->
                if (!hidden && leasedColumns > 0) {
                    try {
                        sessions.leasePaneSize(paneId, leasedColumns, leasedRows)
                    } catch (cancelled: CancellationException) {
                        throw cancelled
                    } catch (_: Exception) {
                    }
                }
            }
        }
    }

    override fun onCleared() {
        leaseLoopJob?.cancel()
        reLeaseJob?.cancel()
        sessions.setViewedPane(null)
        // viewModelScope is already cancelled here — release + unwatch ride
        // the app scope, in order, so the lease drops before the runtime.
        appScope.launch {
            if (leasedColumns > 0) {
                try {
                    sessions.releasePaneSize(paneId)
                } catch (_: Exception) {
                }
            }
            sessions.closePane(paneId)
        }
    }

    /** The view measured its grid — negotiate the lease with the relay. */
    fun onViewportMeasured(columns: Int, rows: Int) {
        if (columns <= 0) return
        viewModelScope.launch {
            try {
                val (appliedColumns, appliedRows) = sessions.leasePaneSize(paneId, columns, rows)
                leasedColumns = appliedColumns
                leasedRows = appliedRows
            } catch (failure: Exception) {
                lastError.value = failure.message
            }
        }
    }

    /** Special-keys bar — Esc/arrows/Ctrl chords ride `send_keys`. */
    fun sendKeys(keys: List<String>, label: String = keys.joinToString(", ")) {
        viewModelScope.launch {
            try {
                sessions.sendKeys(paneId, keys, label)
            } catch (failure: Exception) {
                lastError.value = failure.message
            }
        }
    }

    /** Typed text + Enter — terminal mode's composer path (`send_input`). */
    fun sendText(text: String) {
        if (text.isEmpty()) return
        viewModelScope.launch {
            try {
                sessions.sendTerminalText(paneId, text)
            } catch (failure: Exception) {
                lastError.value = failure.message
            }
        }
    }

    /**
     * `send_text` — literal injection at the pane cursor, no Enter. The
     * input bar's Send/IME-Send action; the pane's own echo (or lack of
     * it) reflects the text back.
     */
    fun sendLiteralText(text: String) {
        if (text.isEmpty()) return
        viewModelScope.launch {
            try {
                sessions.sendText(paneId, text)
            } catch (failure: Exception) {
                lastError.value = failure.message
            }
        }
    }

    /**
     * `send_secret` — the hidden-prompt answer. The relay never journals
     * it; a missing `secret_input` capability fails locally with
     * [CommandException] before anything leaves the device.
     */
    fun sendSecret(text: String) {
        if (text.isEmpty()) return
        viewModelScope.launch {
            try {
                sessions.sendSecret(paneId, text)
            } catch (failure: Exception) {
                lastError.value = failure.message
            }
        }
    }

    /** The snackbar consumed the error — clear so a repeat re-triggers. */
    fun dismissError() {
        lastError.value = null
    }

    /**
     * `pane_link_resolve` — hit-test a viewport cell for a link the served
     * text cannot expose (OSC8). True when herdr reports cell regions; any
     * failure (capability gone, stale frame) means "no server link here".
     */
    suspend fun paneLinkRegions(row: Int, col: Int): Boolean =
        try {
            sessions.paneLinkResolve(paneId, row, col).regions.isNotEmpty()
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            false
        }

    /**
     * `pane_link_activate` — open the link under a viewport cell. The pane
     * host's browser takes it when `handled`; the returned `url` still
     * lets the caller open it locally. Failures surface via [lastError].
     */
    suspend fun activatePaneLink(row: Int, col: Int): PaneLinkActivatedResult? =
        try {
            sessions.paneLinkActivate(paneId, row, col)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            lastError.value = failure.message
            null
        }

    /**
     * `pane_search` — full-scrollback find. The find bar only wants the
     * server's hit count beyond the rendered buffer, so failures fold to
     * null and the local matcher stays the source of truth.
     */
    suspend fun paneSearch(query: String): PaneSearchResult? =
        try {
            sessions.paneSearch(paneId, query)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            null
        }

    /** Pull-to-refresh — the gate coalesces non-forced reads at 35 s. */
    fun refresh() {
        viewModelScope.launch { sessions.refreshPane(paneId) }
    }

    private fun breadcrumbOf(agent: Agent?): String {
        if (agent == null) return ""
        val project = agent.project ?: agent.cwd?.substringAfterLast('/')
        return listOfNotNull(
            project?.takeIf { it.isNotEmpty() },
            agent.sessionName?.takeIf { it.isNotEmpty() },
            agent.relayLabel.takeIf { it.isNotEmpty() },
        ).joinToString(" · ")
    }

    private companion object {
        /** The oracle's `PANE_SIZE_LEASE_REFRESH_MS` — 10 s against a ~120 s TTL. */
        const val LEASE_REFRESH_MS = 10_000L
    }
}
