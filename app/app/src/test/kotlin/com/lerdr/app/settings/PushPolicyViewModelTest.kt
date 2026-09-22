package com.lerdr.app.settings

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.session.FakeCredentialStore
import com.lerdr.app.session.FakeRelaySessionFactory
import com.lerdr.app.session.FakeRelaySessionHandle
import com.lerdr.app.session.SessionRepository
import java.io.File
import java.time.Instant
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import lerdr.core.data.RelayEndpoint
import lerdr.core.data.RelayRegistry
import lerdr.core.data.RelayTransport
import lerdr.core.protocol.LerdrJson
import lerdr.core.store.AgentStore
import lerdr.core.store.ConnectionStore
import lerdr.core.store.WorkspaceStore
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

private fun json(raw: String): JsonObject =
    LerdrJson.parseToJsonElement(raw) as JsonObject

private fun sentFrames(h: FakeRelaySessionHandle): List<JsonObject> =
    h.sentRaw.map { json(it) }

/**
 * [PushPolicyViewModel] against a real [SessionRepository] with in-memory
 * session/store fakes — the wire frames are asserted verbatim.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class PushPolicyViewModelTest {

    @get:Rule
    val tmp = TemporaryFolder()

    private val mainDispatcher = UnconfinedTestDispatcher()

    /** viewModelScope rides Dispatchers.Main — redirect it into the test. */
    @Before
    fun setMain() = Dispatchers.setMain(mainDispatcher)

    @After
    fun resetMain() = Dispatchers.resetMain()

    private class Harness(
        private val testScope: TestScope,
        tmpDir: File,
        private val clock: () -> Long,
    ) {
        private val scope = testScope.backgroundScope
        val credentials = FakeCredentialStore()
        private val relayStore = PreferenceDataStoreFactory.create(scope = scope) {
            File(tmpDir, "relays.preferences_pb")
        }
        val registry = RelayRegistry(relayStore, scope)
        val agents = AgentStore(scope)
        val workspaces = WorkspaceStore()
        val connections = ConnectionStore(clock = { 0L })
        val factory = FakeRelaySessionFactory(scope)
        val repository = SessionRepository(
            scope = scope,
            credentialStore = credentials,
            relayRegistry = registry,
            agentStore = agents,
            workspaceStore = workspaces,
            connectionStore = connections,
            sessionFactory = factory,
        )

        val endpoint = RelayEndpoint(
            id = "r1",
            label = "workstation",
            host = "192.168.1.5",
            port = 7474,
            transport = RelayTransport.WEBSOCKET,
        )
        fun viewModel() = PushPolicyViewModel(repository, clock = clock)

        fun pump() = testScope.runCurrent()

        fun handle(): FakeRelaySessionHandle =
            factory.handleFor(endpoint.socketOrigin)
                ?: error("no session for ${endpoint.socketOrigin}")

        /** CONNECTED + `push_config` carrying the given capability list. */
        suspend fun connectReady(capabilities: List<String> = listOf("push_policy")) {
            repository.connect(endpoint)
            handle().connect()
            val caps = capabilities.joinToString(",") { "\"$it\"" }
            handle().emit(
                json(
                    """{"type":"push_config","protocol":3,"capabilities":[$caps],""" +
                        """"inventory":{"state":"ready"}}""",
                ),
            )
            pump()
        }

        fun sentOf(type: String): List<JsonObject> =
            sentFrames(handle()).filter {
                it["type"]?.jsonPrimitive?.content == type
            }

        /** Poll until [count] frames of [type] were sent — the wire is async. */
        fun awaitSent(type: String, count: Int = 1): List<JsonObject> {
            val deadline = System.currentTimeMillis() + 5_000
            while (sentOf(type).size < count && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            val found = sentOf(type)
            check(found.size >= count) {
                "expected $count '$type' frames, saw ${found.size}"
            }
            return found
        }

        suspend fun emitPolicy(policyJson: String) {
            handle().emit(json("""{"type":"push_policy","policy":$policyJson}"""))
            pump()
        }

        suspend fun emitPolicyResult(ok: Boolean, code: String? = null, policyJson: String? = null) {
            val codeField = code?.let { ""","code":"$it"""" } ?: ""
            val policyField = policyJson?.let { ""","policy":$it""" } ?: ""
            handle().emit(
                json("""{"type":"push_policy_result","ok":$ok$codeField$policyField}"""),
            )
            pump()
        }

        suspend fun emitCommandResult(requestId: String, ok: Boolean, error: String? = null) {
            val errorField = error?.let { ""","error":"$it"""" } ?: ""
            handle().emit(
                json(
                    """{"type":"command_result","request_id":"$requestId","ok":$ok,""" +
                        """"phase":"${if (ok) "completed" else "failed"}"$errorField}""",
                ),
            )
            pump()
        }

        suspend fun emitTestResult(stage: String) {
            handle().emit(json("""{"type":"push_test_result","stage":"$stage"}"""))
            pump()
        }

        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }
    }

    private fun defaultPolicyJson(
        deviceId: String = "dev-1",
        categories: String =
            """{"attention":true,"question":true,"brief":true,"finished":false,"update":true,"test":true}""",
        settleMs: Long = 2_000,
        cooldownMs: Long = 30_000,
        snoozed: Boolean = false,
        snoozeUntil: String? = null,
        updateOnce: Boolean = true,
        locale: String = "en",
    ): String {
        val until = snoozeUntil?.let { ""","snooze_until":"$it"""" } ?: ""
        return """{"device_id":"$deviceId","locale":"$locale","categories":$categories,""" +
            """"settle_ms":$settleMs,"cooldown_ms":$cooldownMs,"snoozed":$snoozed,""" +
            """"update_once":$updateOnce$until}"""
    }

    /** Bound, connected, policy loaded — the state every edit test starts from. */
    private suspend fun Harness.bindReady(
        vm: PushPolicyViewModel,
        policyJson: String = defaultPolicyJson(),
    ) {
        vm.bind("r1")
        connectReady()
        val get = awaitSent("push_policy_get").last()
        emitPolicy(policyJson)
        // The relay's terminal `action_receipt` resolves the get request.
        handle().emit(
            json(
                """{"type":"action_receipt","request_id":"${get["request_id"]!!.jsonPrimitive.content}",""" +
                    """"receipt":{"action_id":"","phase":"confirmed"}}""",
            ),
        )
        pump()
    }

    @Test
    fun `connecting a capable relay requests the policy and adopts the reply`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.connectReady()

        val get = h.awaitSent("push_policy_get").single()
        assertThat(get["protocol"]?.jsonPrimitive?.content).isEqualTo("3")
        h.emitPolicy(
            defaultPolicyJson(
                categories = """{"attention":true,"question":false,"finished":true,"test":false}""",
                settleMs = 5_000,
                cooldownMs = 60_000,
                snoozed = true,
                snoozeUntil = "2030-01-01T00:00:00Z",
                updateOnce = false,
            ),
        )

        val state = vm.uiState.value
        assertThat(state.connected).isTrue()
        assertThat(state.supported).isTrue()
        assertThat(state.relayLabel).isEqualTo("workstation")
        val policy = state.policy!!
        assertThat(policy.deviceId).isEqualTo("dev-1")
        assertThat(policy.categories["attention"]).isTrue()
        assertThat(policy.categories["question"]).isFalse()
        // Absent keys fall back to the oracle's defaults (brief/update on).
        assertThat(policy.categories["brief"]).isTrue()
        assertThat(policy.categories["update"]).isTrue()
        assertThat(policy.categories["finished"]).isTrue()
        assertThat(policy.categories["test"]).isFalse()
        assertThat(policy.settleMs).isEqualTo(5_000)
        assertThat(policy.cooldownMs).isEqualTo(60_000)
        assertThat(policy.snoozed).isTrue()
        assertThat(policy.snoozeUntil).isEqualTo("2030-01-01T00:00:00Z")
        assertThat(policy.updateOnce).isFalse()
    }

    @Test
    fun `a relay without the push_policy capability never requests`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.connectReady(capabilities = listOf("device_management"))

        h.pump()
        assertThat(h.sentOf("push_policy_get")).isEmpty()
        val state = vm.uiState.value
        assertThat(state.connected).isTrue()
        assertThat(state.capabilitiesKnown).isTrue()
        assertThat(state.supported).isFalse()
        assertThat(state.loading).isFalse()
    }

    @Test
    fun `an offline relay shows disconnected state and sends nothing`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.pump()

        val state = vm.uiState.value
        assertThat(state.connected).isFalse()
        assertThat(state.connecting).isFalse()
        assertThat(state.policy).isNull()
        assertThat(h.factory.created).isEmpty()
    }

    @Test
    fun `the policy fetch refires after a reconnect`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)
        assertThat(h.sentOf("push_policy_get")).hasSize(1)

        h.handle().disconnect()
        h.pump()
        assertThat(vm.uiState.value.connected).isFalse()

        // Capabilities persist on the row — the next CONNECTED edge refetches.
        h.handle().connect()
        h.pump()
        h.awaitSent("push_policy_get", count = 2)
        assertThat(h.sentOf("push_policy_get")).hasSize(2)
    }

    @Test
    fun `a category toggle sends the whole editable policy map`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.setCategory("finished", true)
        val sent = h.awaitSent("push_policy_set").last()
        val policy = sent["policy"]!!.jsonObject
        // The untouched keys ride along — whole-map replace semantics.
        val categories = policy["categories"]!!.jsonObject
        assertThat(categories["attention"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(categories["finished"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(categories["test"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(policy["settle_ms"]!!.jsonPrimitive.content).isEqualTo("2000")
        assertThat(policy["cooldown_ms"]!!.jsonPrimitive.content).isEqualTo("30000")
        assertThat(policy["snoozed"]!!.jsonPrimitive.content).isEqualTo("false")
        assertThat(policy["update_once"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(policy.containsKey("snooze_until")).isFalse()

        // Optimistic state already applied; confirmation clears `saving`.
        assertThat(vm.uiState.value.policy!!.categories["finished"]).isTrue()
        h.emitCommandResult(sent["request_id"]!!.jsonPrimitive.content, ok = true)
        h.emitPolicyResult(ok = true, policyJson = defaultPolicyJson(
            categories = """{"attention":true,"question":true,"brief":true,"finished":true,"update":true,"test":true}""",
        ))
        val state = vm.uiState.value
        assertThat(state.saving).isFalse()
        assertThat(state.policyError).isNull()
        assertThat(state.policy!!.categories["finished"]).isTrue()
    }

    @Test
    fun `a rejected set reverts the edit and surfaces the wire code`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.setSettleMs(15_000)
        assertThat(vm.uiState.value.policy!!.settleMs).isEqualTo(15_000)
        val sent = h.awaitSent("push_policy_set").last()

        h.emitCommandResult(
            sent["request_id"]!!.jsonPrimitive.content,
            ok = false,
            error = "Notification policy was rejected",
        )
        h.emitPolicyResult(ok = false, code = "push_invalid_duration")

        val state = vm.uiState.value
        assertThat(state.saving).isFalse()
        assertThat(state.policy!!.settleMs).isEqualTo(2_000)
        assertThat(state.policyError).contains("push_invalid_duration")
    }

    @Test
    fun `a timed snooze sends snoozed with an RFC3339 snooze_until`() = runTest {
        val nowMs = 1_700_000_000_000L
        val h = Harness(this, tmp.root) { nowMs }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.snoozeFor(3_600_000)
        val sent = h.awaitSent("push_policy_set").last()
        val policy = sent["policy"]!!.jsonObject
        assertThat(policy["snoozed"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(policy["snooze_until"]!!.jsonPrimitive.content)
            .isEqualTo(Instant.ofEpochMilli(nowMs).plusMillis(3_600_000).toString())
    }

    @Test
    fun `indefinite snooze omits snooze_until and clearing unsets snoozed`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(
            vm,
            defaultPolicyJson(snoozed = true, snoozeUntil = "2030-01-01T00:00:00Z"),
        )

        vm.snoozeIndefinitely()
        var sent = h.awaitSent("push_policy_set", 1).last()
        var policy = sent["policy"]!!.jsonObject
        assertThat(policy["snoozed"]!!.jsonPrimitive.content).isEqualTo("true")
        assertThat(policy.containsKey("snooze_until")).isFalse()
        h.emitCommandResult(sent["request_id"]!!.jsonPrimitive.content, ok = true)
        h.emitPolicyResult(ok = true, policyJson = defaultPolicyJson(snoozed = true))
        assertThat(vm.uiState.value.policy!!.snoozed).isTrue()
        assertThat(vm.uiState.value.policy!!.snoozeUntil).isNull()

        vm.clearSnooze()
        sent = h.awaitSent("push_policy_set", 2).last()
        policy = sent["policy"]!!.jsonObject
        assertThat(policy["snoozed"]!!.jsonPrimitive.content).isEqualTo("false")
        assertThat(policy.containsKey("snooze_until")).isFalse()
    }

    @Test
    fun `update_once rides the policy set payload`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.setUpdateOnce(false)
        val sent = h.awaitSent("push_policy_set").last()
        assertThat(sent["policy"]!!.jsonObject["update_once"]!!.jsonPrimitive.content)
            .isEqualTo("false")
    }

    @Test
    fun `the test button sends push_test_device and maps a queued stage`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.sendTest()
        assertThat(vm.uiState.value.test).isEqualTo(PushTestUi.Sending)
        val sent = h.awaitSent("push_test_device").last()
        h.emitTestResult("queued")
        h.emitReceiptFor(sent)

        assertThat(vm.uiState.value.test).isEqualTo(PushTestUi.Accepted("queued"))
    }

    @Test
    fun `a rate_limited test stage surfaces as rejected`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        h.bindReady(vm)

        vm.sendTest()
        h.awaitSent("push_test_device")
        h.emitTestResult("rate_limited")

        assertThat(vm.uiState.value.test).isEqualTo(PushTestUi.Rejected("rate_limited"))
    }

    @Test
    fun `a test send while offline reports disconnected`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.pump()

        vm.sendTest()
        assertThat(vm.uiState.value.test).isEqualTo(PushTestUi.Rejected("disconnected"))
    }

    @Test
    fun `a policy frame without device_id is ignored`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.connectReady()
        h.awaitSent("push_policy_get")

        h.emitPolicy("""{"categories":{"attention":false},"settle_ms":500}""")
        assertThat(vm.uiState.value.policy).isNull()
        assertThat(vm.uiState.value.loading).isTrue()
    }

    @Test
    fun `a failed get leaves a retryable error state`() = runTest {
        val h = Harness(this, tmp.root) { 0L }
        val vm = h.viewModel()
        backgroundScope.launch { vm.uiState.collect { } }
        vm.bind("r1")
        h.connectReady()
        val get = h.awaitSent("push_policy_get").last()

        // The relay's failed receipt resolves the request as an error.
        h.handle().emit(
            json(
                """{"type":"action_receipt","request_id":"${get["request_id"]!!.jsonPrimitive.content}",""" +
                    """"receipt":{"action_id":"","phase":"failed_before_dispatch","error":{"code":"unknown_action"}}}""",
            ),
        )
        h.pump()

        assertThat(vm.uiState.value.loadFailed).isTrue()
        assertThat(vm.uiState.value.loading).isFalse()

        vm.refreshPolicy()
        h.awaitSent("push_policy_get", count = 2)
    }

    private suspend fun Harness.emitReceiptFor(frame: JsonObject) {
        handle().emit(
            json(
                """{"type":"action_receipt","request_id":"${frame["request_id"]!!.jsonPrimitive.content}",""" +
                    """"receipt":{"action_id":"","phase":"confirmed"}}""",
            ),
        )
        pump()
    }
}
