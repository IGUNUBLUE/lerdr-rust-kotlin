package com.lerdr.app.home

import androidx.compose.runtime.Immutable
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.lerdr.app.session.DirectoryListing
import com.lerdr.app.session.SessionRepository
import java.text.Normalizer
import kotlin.math.max
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.model.AgentProfile
import lerdr.core.store.Agent
import lerdr.core.store.AgentInventoryState
import lerdr.core.store.RelayConnection
import lerdr.core.store.RelayStatus
import lerdr.core.store.WorkspaceStore
import lerdr.core.transport.CommandException

/** A ready relay offered in the launch form's "Computer" picker. */
@Immutable
data class LaunchRelayOption(
    val id: String,
    val label: String,
)

/** A workspace the new agent can join; [id] empty means "new workspace". */
@Immutable
data class LaunchWorkspaceOption(
    val id: String,
    val label: String,
)

/** `list_directories` browser state — the dialog renders these directly. */
@Immutable
data class DirectoryBrowserUi(
    val open: Boolean = false,
    /** The selected relay advertised `directory_browser`. */
    val supported: Boolean = true,
    val loading: Boolean = false,
    val listing: DirectoryListing? = null,
    val error: String? = null,
)

/**
 * Both launch sheets' projected state — the oracle's LaunchView +
 * WorkspaceManager create dialog flattened into one form model (they share
 * the relay picker, cwd, and the directory browser).
 */
@Immutable
data class LaunchUiState(
    /** Connected relays with a `ready` agent inventory (oracle `connectedRelays`). */
    val relays: List<LaunchRelayOption> = emptyList(),
    /** Connected-but-not-ready labels — "Agent inventory is unavailable on …". */
    val unavailableRelayLabels: List<String> = emptyList(),
    val relayId: String = "",
    /** `!canControl(relayId)` — submit disabled + the oracle's read-only warning. */
    val readOnly: Boolean = false,
    // ── agent form ──────────────────────────────────────────────────
    val profiles: List<AgentProfile> = emptyList(),
    val profileId: String = "",
    val name: String = "",
    val prompt: String = "",
    /** Existing workspaces on the selected relay — the tab target picker. */
    val workspaces: List<LaunchWorkspaceOption> = emptyList(),
    val workspaceId: String = "",
    /** Display label of [workspaceId] — drives the "New tab in workspace" hint. */
    val workspaceTargetLabel: String = "",
    // ── shared cwd + directory browser ─────────────────────────────
    val cwd: String = "",
    val cwdLabel: String = "",
    /**
     * A `list_directories` result landed for [relayId] — the oracle's
     * `directoryRelayId === relayId` submit gate.
     */
    val directoryReady: Boolean = false,
    val directory: DirectoryBrowserUi = DirectoryBrowserUi(),
    // ── workspace form ──────────────────────────────────────────────
    val workspaceLabel: String = "",
    // ── shared status ───────────────────────────────────────────────
    val submitting: Boolean = false,
    val status: String? = null,
    val statusError: Boolean = false,
)

/**
 * Launch flows behind the Home FAB — `agent_start` ("New agent") and
 * `workspace_create` ("New workspace"), both ports of the oracle's
 * LaunchView / WorkspaceManager create dialog onto a modal sheet.
 *
 * The VM lives on Home's NavEntry scope so a submit's `waitForAgent`
 * survives sheet dismissal; results arrive on [events]/[messages].
 */
class LaunchViewModel(
    private val sessions: SessionRepository,
    private val workspaces: WorkspaceStore,
) : ViewModel() {

    /** One-shot results for the sheet host — dismiss, then maybe navigate. */
    sealed interface LaunchEvent {
        /** The agent materialized in the store — open its feed. */
        data class Launched(val paneId: String) : LaunchEvent

        /** Submit finished — close the sheet. */
        data object Dismissed : LaunchEvent
    }

    /** Mutable form fields + in-flight browser/submit state. */
    private data class Draft(
        val relayId: String = "",
        val profileId: String = "",
        val name: String = "",
        val prompt: String = "",
        val workspaceId: String = "",
        val workspaceLabel: String = "",
        val cwd: String = "",
        val cwdLabel: String = "",
        /** Relay the current listing belongs to — the oracle's `directoryRelayId`. */
        val directoryRelayId: String = "",
        val directory: DirectoryBrowserUi = DirectoryBrowserUi(),
        val submitting: Boolean = false,
        val status: String? = null,
        val statusError: Boolean = false,
    )

    private val draft = MutableStateFlow(Draft())
    private val _events = MutableSharedFlow<LaunchEvent>(extraBufferCapacity = 4)
    val events: SharedFlow<LaunchEvent> = _events.asSharedFlow()
    private val _messages = MutableSharedFlow<String>(extraBufferCapacity = 4)
    val messages: SharedFlow<String> = _messages.asSharedFlow()

    /** Generation guard — a stale `list_directories` answer is dropped. */
    private var directoryLoadGeneration = 0L

    /** True between `begin*` and the next app pass — gates relay auto-pick. */
    private var sheetActive = false

    /**
     * `Eagerly`, not WhileSubscribed — `submitAgent`/`submitWorkspace`/
     * `openDirectoryBrowser` read [uiState].value themselves, so the
     * projection must be live even before the sheet's first composition.
     */
    val uiState: StateFlow<LaunchUiState> = combine(
        sessions.relays,
        sessions.connections,
        workspaces.workspaces,
        draft,
        ::project,
    ).stateIn(viewModelScope, SharingStarted.Eagerly, LaunchUiState())

    init {
        // The oracle's $effect: keep the picked relay valid while a sheet is
        // open — a disconnecting relay falls back to the first ready one.
        viewModelScope.launch {
            combine(sessions.relays, sessions.connections, ::Pair).collect {
                (relays, connections) ->
                if (!sheetActive) return@collect
                val ready = readyRelays(relays, connections)
                if (ready.none { it.id == draft.value.relayId }) {
                    ready.firstOrNull()?.let { selectRelay(it.id) }
                }
            }
        }
    }

    /** Sheet opened for `agent_start` — fresh fields, warm directory. */
    fun beginAgent() {
        sheetActive = true
        draft.update {
            it.copy(
                name = "",
                prompt = "",
                workspaceId = "",
                status = null,
                statusError = false,
                submitting = false,
            )
        }
        ensureRelay()
    }

    /** Sheet opened for `workspace_create` — label seeds from the cwd. */
    fun beginWorkspace() {
        sheetActive = true
        draft.update {
            it.copy(
                workspaceLabel = it.workspaceLabel.ifBlank { pathBase(it.cwd) },
                status = null,
                statusError = false,
                submitting = false,
            )
        }
        ensureRelay()
    }

    fun selectRelay(relayId: String) {
        if (relayId == draft.value.relayId) return
        directoryLoadGeneration++
        draft.update {
            it.copy(
                relayId = relayId,
                profileId = "",
                name = "",
                workspaceId = "",
                workspaceLabel = "",
                cwd = "",
                cwdLabel = "",
                directoryRelayId = "",
                status = null,
            )
        }
        loadDirectory("")
    }

    fun selectProfile(profileId: String) {
        draft.update {
            it.copy(
                profileId = profileId,
                name = suggestedLaunchName(it.cwd, profileId),
            )
        }
    }

    fun selectWorkspace(workspaceId: String) {
        draft.update { it.copy(workspaceId = workspaceId) }
    }

    /** The oracle's name field: maxlength 32, `[a-z][a-z0-9_-]{0,31}`. */
    fun onNameChange(value: String) {
        draft.update { it.copy(name = value.take(NAME_MAX)) }
    }

    fun onPromptChange(value: String) {
        draft.update { it.copy(prompt = value.take(PROMPT_MAX)) }
    }

    fun onWorkspaceLabelChange(value: String) {
        draft.update { it.copy(workspaceLabel = value.take(WORKSPACE_LABEL_MAX)) }
    }

    fun openDirectoryBrowser() {
        val state = uiState.value
        if (state.relayId.isEmpty() || !state.directory.supported) {
            // No ready relay, or the relay dropped `directory_browser`
            // mid-form — open with the unsupported pane and invalidate any
            // in-flight load instead of spinning forever.
            directoryLoadGeneration++
            draft.update {
                it.copy(
                    directory = it.directory.copy(
                        open = true,
                        supported = false,
                        loading = false,
                    ),
                )
            }
            return
        }
        draft.update { it.copy(directory = it.directory.copy(open = true)) }
        if (draft.value.directoryRelayId != state.relayId &&
            !draft.value.directory.loading
        ) {
            loadDirectory(draft.value.cwd)
        }
    }

    fun closeDirectoryBrowser() {
        draft.update { it.copy(directory = it.directory.copy(open = false)) }
    }

    /**
     * `list_directories` — oracle `loadDirectory`: the browsed folder becomes
     * the selected cwd on success; the suggested name follows it.
     */
    fun loadDirectory(path: String) {
        val relayId = draft.value.relayId.ifEmpty { uiState.value.relayId }
        val connection = sessions.connections.value[relayId] ?: return
        if (relayId.isEmpty()) return
        if (DIRECTORY_BROWSER_CAPABILITY !in connection.capabilities) {
            draft.update {
                it.copy(directory = it.directory.copy(supported = false, loading = false))
            }
            return
        }
        val generation = ++directoryLoadGeneration
        draft.update {
            it.copy(
                directory = it.directory.copy(
                    supported = true,
                    loading = true,
                    error = null,
                ),
            )
        }
        viewModelScope.launch {
            try {
                val listing = sessions.listDirectories(relayId, path)
                if (generation != directoryLoadGeneration) return@launch
                draft.update { d ->
                    d.copy(
                        cwd = listing.currentPath,
                        cwdLabel = listing.currentLabel,
                        directoryRelayId = relayId,
                        name = suggestedLaunchName(listing.currentPath, profileId(d)),
                        workspaceLabel = d.workspaceLabel.ifBlank {
                            pathBase(listing.currentPath)
                        },
                        directory = d.directory.copy(
                            loading = false,
                            listing = listing,
                            error = null,
                        ),
                    )
                }
            } catch (failure: Exception) {
                if (generation != directoryLoadGeneration) return@launch
                draft.update {
                    it.copy(
                        directory = it.directory.copy(
                            loading = false,
                            error = failure.message ?: "Directory listing failed.",
                        ),
                    )
                }
            }
        }
    }

    /**
     * `agent_start` — the oracle's submit: gated on a directory listing
     * landed for this relay, a valid profile, and a name matching
     * `[a-z][a-z0-9_-]{0,31}`. On success the sheet closes, a toast reports
     * the wire warning (or "Agent started."), and `waitForAgent` navigates
     * to the new pane's feed when it appears.
     */
    fun submitAgent() {
        val state = uiState.value
        if (state.relayId.isEmpty() || state.readOnly || state.submitting ||
            state.directory.loading || !state.directoryReady ||
            state.profileId.isEmpty() || state.cwd.isEmpty() ||
            !validAgentName(state.name)
        ) {
            return
        }
        val launchName = state.name.trim()
        val launchCwd = state.cwd.trim()
        draft.update { it.copy(submitting = true, status = "Starting agent…", statusError = false) }
        viewModelScope.launch {
            try {
                val result = sessions.startAgent(
                    relayId = state.relayId,
                    profileId = state.profileId,
                    name = launchName,
                    cwd = launchCwd,
                    prompt = state.prompt,
                    workspaceId = state.workspaceId,
                )
                val warning = result.data?.jsonObject?.get("warning")
                    ?.takeIf { it is kotlinx.serialization.json.JsonPrimitive && it.isString }
                    ?.jsonPrimitive?.content.orEmpty()
                val status = warning.ifEmpty { "Agent started." }
                draft.update {
                    it.copy(
                        submitting = false,
                        status = status,
                        statusError = warning.isNotEmpty(),
                        name = "",
                        prompt = "",
                    )
                }
                _messages.emit(status)
                _events.emit(LaunchEvent.Dismissed)
                val rawPaneId = result.data?.jsonObject?.get("pane_id")
                    ?.takeIf { it is kotlinx.serialization.json.JsonPrimitive && it.isString }
                    ?.jsonPrimitive?.content.orEmpty()
                waitForAgent(state.relayId, rawPaneId, launchName, launchCwd)
                    ?.let { _events.emit(LaunchEvent.Launched(it.paneId)) }
            } catch (failure: Exception) {
                val message = failure.message ?: "Could not start the agent."
                draft.update {
                    it.copy(submitting = false, status = message, statusError = true)
                }
                _messages.emit(message)
            }
        }
    }

    /**
     * `workspace_create` — the oracle's create dialog: on a
     * `dispatched_unknown` the sheet closes and warns against a blind retry;
     * other failures stay in the form.
     */
    fun submitWorkspace() {
        val state = uiState.value
        if (state.relayId.isEmpty() || state.readOnly || state.submitting ||
            state.cwd.isEmpty() || state.workspaceLabel.isBlank()
        ) {
            return
        }
        val label = state.workspaceLabel.trim()
        draft.update { it.copy(submitting = true, status = null, statusError = false) }
        viewModelScope.launch {
            try {
                sessions.createWorkspace(state.relayId, state.cwd.trim(), label)
                draft.update {
                    it.copy(
                        submitting = false,
                        status = "Created workspace $label.",
                        workspaceLabel = "",
                    )
                }
                _messages.emit("Created workspace $label.")
                _events.emit(LaunchEvent.Dismissed)
            } catch (failure: Exception) {
                if ((failure as? CommandException)?.dispatchedUnknown == true) {
                    draft.update { it.copy(submitting = false) }
                    _messages.emit(
                        "${failure.message} Check the workspace list before retrying.",
                    )
                    _events.emit(LaunchEvent.Dismissed)
                } else {
                    draft.update {
                        it.copy(
                            submitting = false,
                            status = failure.message ?: "Could not create the workspace.",
                            statusError = true,
                        )
                    }
                }
            }
        }
    }

    /**
     * The oracle's `waitForAgent` — match on `raw_pane_id` first, else on
     * name (agent or tab label) + cwd; refresh once, give up after 6 s.
     */
    private suspend fun waitForAgent(
        relayId: String,
        rawPaneId: String,
        name: String,
        cwd: String,
    ): Agent? {
        val match: (Agent) -> Boolean = { agent ->
            when {
                agent.relayId != relayId -> false
                rawPaneId.isNotEmpty() && agent.rawPaneId == rawPaneId -> true
                name.isEmpty() ||
                    (agent.name != name && agent.tabLabel != name) -> false
                else -> cwd.isEmpty() || agent.cwd.isNullOrEmpty() || agent.cwd == cwd
            }
        }
        sessions.agents.value.firstOrNull(match)?.let { return it }
        sessions.refreshAgents()
        return withTimeoutOrNull(WAIT_FOR_AGENT_MS) {
            sessions.agents.first { agents -> agents.any(match) }
        }?.firstOrNull(match)
    }

    private fun ensureRelay() {
        val ready = readyRelays(sessions.relays.value, sessions.connections.value)
        when {
            ready.any { it.id == draft.value.relayId } -> {
                if (draft.value.directoryRelayId != draft.value.relayId) loadDirectory("")
            }
            ready.isNotEmpty() -> selectRelay(ready.first().id)
        }
    }

    private fun profileId(draft: Draft): String {
        val profiles = sessions.connections.value[draft.relayId]?.agentProfiles.orEmpty()
        return if (profiles.any { it.id == draft.profileId }) {
            draft.profileId
        } else {
            profiles.firstOrNull()?.id.orEmpty()
        }
    }

    private fun project(
        relays: List<RelayEndpoint>,
        connections: Map<String, RelayConnection>,
        workspaceRows: List<lerdr.core.store.RelayWorkspace>,
        d: Draft,
    ): LaunchUiState {
        val ready = readyRelays(relays, connections)
        val unavailable = relays.filter { relay ->
            val connection = connections[relay.id]
            connection?.status == RelayStatus.CONNECTED &&
                connection.inventory.state != AgentInventoryState.READY
        }
        val relayId = if (ready.any { it.id == d.relayId }) {
            d.relayId
        } else {
            ready.firstOrNull()?.id.orEmpty()
        }
        val connection = connections[relayId]
        val profiles = connection?.agentProfiles.orEmpty()
        val profileId = if (profiles.any { it.id == d.profileId }) {
            d.profileId
        } else {
            profiles.firstOrNull()?.id.orEmpty()
        }
        val workspaceOptions = workspaceRows.filter { it.relayId == relayId }
        val workspaceId = if (workspaceOptions.any { it.workspaceId == d.workspaceId }) {
            d.workspaceId
        } else {
            ""
        }
        return LaunchUiState(
            relays = ready.map { LaunchRelayOption(it.id, it.label) },
            unavailableRelayLabels = unavailable.map { it.label },
            relayId = relayId,
            readOnly = relayId.isEmpty() || !sessions.canControl(relayId),
            profiles = profiles,
            profileId = profileId,
            name = d.name,
            prompt = d.prompt,
            workspaces = workspaceOptions.map {
                LaunchWorkspaceOption(it.workspaceId, it.label)
            },
            workspaceId = workspaceId,
            workspaceTargetLabel = workspaceOptions
                .firstOrNull { it.workspaceId == workspaceId }?.label.orEmpty(),
            cwd = d.cwd,
            cwdLabel = d.cwdLabel,
            directoryReady = d.directoryRelayId == relayId && d.cwd.isNotEmpty(),
            directory = run {
                val supported = connection?.capabilities
                    ?.contains(DIRECTORY_BROWSER_CAPABILITY) == true
                d.directory.copy(
                    supported = supported,
                    // A revoked capability can't still be loading.
                    loading = d.directory.loading && supported,
                )
            },
            workspaceLabel = d.workspaceLabel,
            submitting = d.submitting,
            status = d.status,
            statusError = d.statusError,
        )
    }

    /** Connected + inventory ready — the oracle's `connectedRelays` filter. */
    private fun readyRelays(
        relays: List<RelayEndpoint>,
        connections: Map<String, RelayConnection>,
    ): List<RelayEndpoint> = relays.filter { relay ->
        val connection = connections[relay.id]
        connection?.status == RelayStatus.CONNECTED &&
            connection.inventory.state == AgentInventoryState.READY
    }

    private companion object {
        const val DIRECTORY_BROWSER_CAPABILITY = "directory_browser"
        const val WAIT_FOR_AGENT_MS = 6_000L
        const val NAME_MAX = 32
        const val PROMPT_MAX = 100_000
        const val WORKSPACE_LABEL_MAX = 256
    }
}

/** The oracle's `validAgentName` — `[a-z][a-z0-9_-]{0,31}`. */
internal fun validAgentName(value: String): Boolean =
    Regex("[a-z][a-z0-9_-]{0,31}").matches(value)

private val Diacritics = Regex("\\p{M}+")
private val NonNameChars = Regex("[^a-z0-9_-]+")
private val EdgeSeparators = Regex("^[-_]+|[-_]+$")

/**
 * The oracle's `launchNamePart` — NFKD, strip diacritics, lowercase,
 * non-name chars collapse to `-`, edges trimmed; when the result doesn't
 * start with a letter it is suffixed onto the fallback.
 */
internal fun launchNamePart(value: String, fallback: String): String {
    val normalized = Normalizer.normalize(value, Normalizer.Form.NFKD)
        .replace(Diacritics, "")
        .lowercase()
    val cleaned = normalized
        .replace(NonNameChars, "-")
        .replace(EdgeSeparators, "")
    if (cleaned.isEmpty()) return fallback
    return if (cleaned.first() in 'a'..'z') cleaned else "$fallback-$cleaned"
}

/**
 * The oracle's `suggestedLaunchName` — `<dir-basename>-<profile>` capped to
 * the 32-char wire name, e.g. `/home/u/api-server` + `claude` →
 * `api-server-claude`.
 */
internal fun suggestedLaunchName(cwd: String, profileId: String): String {
    val parts = cwd.trimEnd('/', '\\')
        .split('/', '\\')
        .filter { it.isNotEmpty() }
    val directory = launchNamePart(parts.lastOrNull().orEmpty(), "project")
    val agent = launchNamePart(profileId, "agent")
    val suffix = "-${agent.take(12)}"
    return "${directory.take(max(1, NAME_LIMIT - suffix.length))}$suffix"
}

private const val NAME_LIMIT = 32
