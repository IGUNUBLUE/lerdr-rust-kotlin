package com.lerdr.app.security

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import com.google.common.truth.Truth.assertThat
import com.lerdr.app.settings.AppPreferences
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * Lock semantics — real DataStore on a temp folder (repo convention:
 * preferences are never mocked). `locked` starts fail-closed until the
 * stored flag reads back, so every test awaits the settled value rather
 * than the initial emission.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class LockViewModelTest {

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
    ) {
        private val dataStore = PreferenceDataStoreFactory.create(
            scope = testScope.backgroundScope,
        ) {
            File(tmpDir, "app.preferences_pb")
        }
        val preferences = AppPreferences(dataStore)

        // A fresh LockState per call = a fresh process-scoped latch.
        fun viewModel() = LockViewModel(LockState(preferences))

        /** Poll on a real clock — DataStore IO is off the test scheduler. */
        fun await(condition: () -> Boolean) {
            val deadline = System.currentTimeMillis() + 5_000
            while (!condition() && System.currentTimeMillis() < deadline) {
                testScope.runCurrent()
                Thread.sleep(5)
            }
            check(condition()) { "condition not met within deadline" }
        }

        /** Let the stored preference propagate before asserting a state. */
        fun settle() {
            Thread.sleep(300)
            testScope.runCurrent()
        }
    }

    @Test
    fun `starts locked when the setting is enabled`() = runTest {
        val h = Harness(this, tmp.root)
        h.preferences.setAppLockEnabled(true)

        val viewModel = h.viewModel()
        // Fail-closed: the gate is locked before the preference read
        // lands — content never flashes open on a cold start.
        assertThat(viewModel.locked.value).isTrue()

        h.settle()
        assertThat(viewModel.locked.value).isTrue()
    }

    @Test
    fun `disabled setting settles unlocked and cannot lock`() = runTest {
        val h = Harness(this, tmp.root)
        val viewModel = h.viewModel()

        // Past the fail-closed start the stored `false` opens the gate.
        h.await { !viewModel.locked.value }

        // Manual re-lock and a stray unlock both stay open — the gate
        // is off entirely.
        viewModel.lock()
        viewModel.unlock()
        h.await { !viewModel.locked.value }
        assertThat(viewModel.locked.value).isFalse()
    }

    @Test
    fun `successful verification unlocks for the rest of the process`() = runTest {
        val h = Harness(this, tmp.root)
        h.preferences.setAppLockEnabled(true)
        val viewModel = h.viewModel()
        h.await { viewModel.locked.value }

        viewModel.unlock()
        h.await { !viewModel.locked.value }
        assertThat(viewModel.locked.value).isFalse()
    }

    @Test
    fun `lock() re-locks an unlocked session while enabled`() = runTest {
        val h = Harness(this, tmp.root)
        h.preferences.setAppLockEnabled(true)
        val viewModel = h.viewModel()
        h.await { viewModel.locked.value }
        viewModel.unlock()
        h.await { !viewModel.locked.value }

        viewModel.lock()
        h.await { viewModel.locked.value }
        assertThat(viewModel.locked.value).isTrue()
    }

    @Test
    fun `disable then enable re-locks even after an unlock`() = runTest {
        val h = Harness(this, tmp.root)
        h.preferences.setAppLockEnabled(true)
        val viewModel = h.viewModel()
        h.await { viewModel.locked.value }
        viewModel.unlock()
        h.await { !viewModel.locked.value }

        // Off clears the session latch…
        h.preferences.setAppLockEnabled(false)
        h.await { !viewModel.locked.value }
        h.settle()

        // …so toggling back on locks instead of reopening the session.
        h.preferences.setAppLockEnabled(true)
        h.await { viewModel.locked.value }
        assertThat(viewModel.locked.value).isTrue()
    }

    @Test
    fun `a new process starts locked even if the last one was unlocked`() = runTest {
        val h = Harness(this, tmp.root)
        h.preferences.setAppLockEnabled(true)
        val first = h.viewModel()
        h.await { first.locked.value }
        first.unlock()
        h.await { !first.locked.value }

        // Fresh ViewModel over the same store = cold start: the latch
        // is process-scoped, so the gate is closed again.
        val second = h.viewModel()
        h.settle()
        assertThat(second.locked.value).isTrue()
    }
}
