package com.lerdr.app.session

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.settings.AppPreferences
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
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.model.ClientCapabilities
import lerdr.core.model.PaneLinkActivatedResult
import lerdr.core.model.PaneSearchResult
import lerdr.core.store.Agent
import lerdr.core.store.RelayStatus
import lerdr.core.store.orchestratingStatus
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
    /** Live agent lifecycle (or derived cohort activity), never viewport geometry. */
    val statusLabel: String = "",
    val connected: Boolean = false,
    val connecting: Boolean = false,
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
     * Lerdr's `readOnly` gate, inverted — mutating affordances
     * (input bar, key chips, secret field) enable only for an enrolled
     * CONTROLLER credential. Fail-closed like Lerdr.
     */
    val canControl: Boolean = false,
    /**
     * Relay `secret_input` capability — the hidden-prompt answer path
     * (`send_secret`). Without it the bar stays in plain mode and the
     * prompt banner carries Lerdr's too-old-relay hint instead.
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
    private val appPreferences: AppPreferences,
) : ViewModel() {

    private val relayId = paneId.substringBefore("::")
    private val lastError = MutableStateFlow<String?>(null)

    /**
     * The lease the relay applied (clamped to its bounds), renewed on the
     * Lerdr's 10 s cadence — the relay TTL is ~120 s and the renewal is
     * also what re-arms the lease after a reconnect dropped it. Volatile:
     * written on viewModelScope, read on appScope.
     */
    @Volatile
    private var leasedColumns = 0

    @Volatile
    private var leasedRows = 0

    /** Last phone grid — reapply after returning to the foreground. */
    @Volatile
    private var measuredColumns = 0

    @Volatile
    private var measuredRows = 0

    // Parse cache keyed on the committed content — a metadata-only delta
    // bumps revision without touching `lines`, so the row list survives
    // unchanged and the renderer keeps its measured draw state.
    private var parsedContent: String? = null
    private var parsedFormat: String = ""
    private var parsedRows: List<TerminalRowUi> = emptyList()
    private var parsedCursor: TerminalCursorUi? = null

    /**
     * Last committed frame outside `resize_settling` — resizing the native
     * terminal makes the TUI repaint mid-frame. Settling frames may carry
     * stale cells mixed into the new layout. They still commit to
     * [PaneSurface] (the delta chain and acks must not skip); only the
     * display holds the previous settled frame until the flag clears.
     */
    private var settledSnapshot: PaneSurface.Snapshot? = null

    val uiState: StateFlow<TerminalUiState> = combine(
        sessions.paneSnapshot(paneId),
        sessions.agent(paneId),
        sessions.connection(relayId),
        sessions.cohortBusy(paneId),
        lastError,
    ) { snapshot, agent, connection, cohortBusy, error ->
        if (snapshot != null && !snapshot.resizeSettling) settledSnapshot = snapshot
        val display = snapshot?.let {
            if (it.resizeSettling) settledSnapshot ?: it else it
        }
        val content = display?.content
        val format = display?.format.orEmpty()
        if (content != parsedContent || format != parsedFormat) {
            parsedContent = content
            parsedFormat = format
            parsedRows = if (display == null) {
                emptyList()
            } else {
                parseTerminalRows(display.lines, format)
            }
            parsedCursor = terminalCursor(parsedRows)
        }
        TerminalUiState(
            paneId = paneId,
            title = agent?.name ?: agent?.agent ?: paneId.substringAfter("::"),
            provider = agent?.agent?.takeIf { it.isNotEmpty() },
            breadcrumb = breadcrumbOf(agent),
            statusLabel = agent.orchestratingStatus(cohortBusy) ?: agent?.status ?: "",
            connected = connection?.status == RelayStatus.CONNECTED,
            connecting = connection?.status == RelayStatus.CONNECTING,
            waitingForContent = display == null,
            lines = display?.lines.orEmpty(),
            rows = parsedRows,
            cursor = parsedCursor,
            revision = display?.revision ?: 0,
            truncated = display?.truncated == true,
            noEcho = display?.noEcho == true,
            noEchoPrompt = display?.noEchoPrompt,
            leaseColumns = display?.columns ?: 0,
            leaseRows = display?.rows ?: 0,
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
    private val paneOwner = Any()

    /**
     * Persisted pinch-zoom — the screen applies it to the surface state;
     * pinch gestures report back through [persistFontScale].
     */
    val terminalFontScale: StateFlow<Float> = appPreferences.terminalFontScale
        .stateIn(viewModelScope, SharingStarted.Eagerly, 1f)

    /** A finite write outlives a quick exit without retaining a closed screen forever. */
    private var fontScaleWriteJob: Job? = null

    fun persistFontScale(scale: Float) {
        fontScaleWriteJob?.cancel()
        fontScaleWriteJob = appScope.launch {
            delay(FONT_SCALE_PERSIST_MS)
            appPreferences.setTerminalFontScale(scale)
        }
    }

    init {
        // push_viewed_pane: the terminal view is Lerdr's "viewed"
        // signal — entering publishes it, leaving clears it.
        sessions.setViewedPane(paneId, paneOwner)
        viewModelScope.launch { sessions.openPane(paneId, paneOwner) }
        leaseLoopJob = appScope.launch {
            while (true) {
                delay(LEASE_REFRESH_MS)
                val columns = leasedColumns
                if (columns > 0 && !sessions.hidden.value && sessions.canControl(relayId) &&
                    sessions.connectionNow(relayId)?.status == RelayStatus.CONNECTED
                ) {
                    try {
                        // Renew our requested grid, not a smaller peer's temporary minimum.
                        val (appliedColumns, appliedRows) =
                            sessions.leasePaneSize(paneId, paneOwner, measuredColumns, measuredRows)
                        leasedColumns = appliedColumns
                        leasedRows = appliedRows
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
                    acquireLease(measuredColumns, measuredRows)
                }
            }
        }
        // A restored terminal may measure before inventory or authenticated control is ready.
        viewModelScope.launch {
            combine(sessions.agent(paneId), sessions.connection(relayId)) { agent, connection ->
                agent != null && connection?.status == RelayStatus.CONNECTED &&
                    sessions.canControl(relayId)
            }.distinctUntilChanged().collect { available ->
                if (available && measuredColumns > 0 && leasedColumns == 0) {
                    acquireLease(measuredColumns, measuredRows)
                }
            }
        }
    }

    override fun onCleared() {
        leaseLoopJob?.cancel()
        reLeaseJob?.cancel()
        sessions.setViewedPane(null, paneOwner)
        // The repository owns in-flight lease state too: even a canceled
        // acquire whose receipt never arrived must release on final close.
        appScope.launch { sessions.closePane(paneId, paneOwner) }
    }

    /** Negotiate the phone grid for every provider, including full-screen CLIs. */
    fun onViewportMeasured(columns: Int, rows: Int) {
        if (columns <= 0) return
        measuredColumns = columns
        measuredRows = rows
        if (sessions.agentNow(paneId) == null) return
        viewModelScope.launch { acquireLease(columns, rows) }
    }

    private suspend fun acquireLease(columns: Int, rows: Int) {
        if (sessions.hidden.value || !sessions.canControl(relayId) ||
            sessions.connectionNow(relayId)?.status != RelayStatus.CONNECTED
        ) return
        try {
            val (appliedColumns, appliedRows) =
                sessions.leasePaneSize(paneId, paneOwner, columns, rows)
            leasedColumns = appliedColumns
            leasedRows = appliedRows
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            lastError.value = failure.message
        }
    }


    /** Special-keys bar — Esc/arrows/Ctrl chords ride `send_keys`. */
    fun sendKeys(keys: List<String>, label: String = keys.joinToString(", ")) {
        viewModelScope.launch {
            deliverInput { sessions.sendKeys(paneId, keys, label) }
        }
    }

    /** Typed text + Enter — terminal mode's composer path (`send_input`). */
    suspend fun sendText(text: String): Boolean =
        text.isNotEmpty() && deliverInput { sessions.sendTerminalText(paneId, text) }

    /** Hidden-prompt delivery succeeds only after the relay acknowledges it. */
    suspend fun sendSecret(text: String): Boolean =
        text.isNotEmpty() && deliverInput { sessions.sendSecret(paneId, text) }

    private suspend fun deliverInput(send: suspend () -> Unit): Boolean {
        if (!sessions.canControl(relayId)) {
            lastError.value = "This device can only observe the terminal."
            return false
        }
        if (sessions.connectionNow(relayId)?.status != RelayStatus.CONNECTED) {
            lastError.value = "Relay offline. Input was not sent."
            return false
        }
        return try {
            send()
            true
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (failure: Exception) {
            lastError.value = if (failure is CommandException && failure.dispatchedUnknown) {
                "Delivery unconfirmed. Check the terminal before sending again."
            } else {
                failure.message ?: "Terminal input failed."
            }
            false
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
        /** Lerdr's `PANE_SIZE_LEASE_REFRESH_MS` — 10 s against a ~120 s TTL. */
        const val LEASE_REFRESH_MS = 10_000L

        /** Settle window before a pinch-zoom value lands in preferences. */
        const val FONT_SCALE_PERSIST_MS = 400L

    }
}
