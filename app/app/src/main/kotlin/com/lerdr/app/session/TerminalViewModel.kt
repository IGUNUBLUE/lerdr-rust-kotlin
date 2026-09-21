package com.lerdr.app.session

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.ui.terminal.TerminalCursorUi
import com.lerdr.app.ui.terminal.TerminalRowUi
import com.lerdr.app.ui.terminal.parseTerminalRows
import com.lerdr.app.ui.terminal.terminalCursor
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import lerdr.core.store.Agent
import lerdr.core.store.RelayStatus
import lerdr.core.terminal.PaneSurface

/** Everything Terminal mode renders — pane snapshot + key/send actions. */
data class TerminalUiState(
    val paneId: String,
    val title: String = "",
    val breadcrumb: String = "",
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
            lastError = error,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), TerminalUiState(paneId))

    init {
        viewModelScope.launch { sessions.openPane(paneId) }
    }

    override fun onCleared() {
        // viewModelScope is already cancelled here — the unwatch rides the app scope.
        appScope.launch { sessions.closePane(paneId) }
    }

    /** The view measured its grid — negotiate the lease with the relay. */
    fun onViewportMeasured(columns: Int, rows: Int) {
        if (columns <= 0) return
        viewModelScope.launch {
            try {
                sessions.leasePaneSize(paneId, columns, rows)
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
}
