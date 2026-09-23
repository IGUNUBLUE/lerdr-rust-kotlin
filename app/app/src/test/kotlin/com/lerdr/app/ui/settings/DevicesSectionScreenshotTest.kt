package com.lerdr.app.ui.settings

import androidx.activity.ComponentActivity
import androidx.compose.foundation.layout.padding
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage
import com.lerdr.app.settings.DeviceUi
import com.lerdr.app.settings.DevicesContent
import com.lerdr.app.settings.DevicesUiState
import com.lerdr.app.settings.InvitationUi
import com.lerdr.app.settings.QrBitmapUi
import com.lerdr.core.designsystem.theme.LerdrTheme
import lerdr.core.data.DeviceRole
import lerdr.core.model.HerdrFeatureStatus
import lerdr.core.model.HerdrStatus
import lerdr.core.model.UpdateState
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * Roborazzi coverage for the Devices settings section — populated list,
 * empty, error status, and the invitation block with its QR.
 */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], application = com.lerdr.app.TestApp::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class DevicesSectionScreenshotTest {

    @get:Rule
    val composeRule = createAndroidComposeRule<ComponentActivity>()

    private val options = RoborazziOptions(
        // Dump (the JVM default) paints a semantics-tree overlay whose
        // node text jitters run-to-run — force a plain bitmap capture.
        captureType = RoborazziOptions.CaptureType.Screenshot(),
        compareOptions = RoborazziOptions.CompareOptions(changeThreshold = 0.02f),
    )

    private fun content(uiState: DevicesUiState) {
        composeRule.setContent {
            LerdrTheme {
                DevicesContent(
                    uiState = uiState,
                    onRefresh = {},
                    onRename = { _, _ -> },
                    onRevoke = {},
                    onInvite = { _, _ -> },
                    onForgetCurrent = {},
                    onReset = {},
                    onInvitationCopied = {},
                    onInvitationCopyFailed = {},
                    onDismissInvitation = {},
                    onDismissStatus = {},
                    onCheckUpdate = {},
                    onInstallUpdate = {},
                    modifier = Modifier.padding(12.dp),
                )
            }
        }
        composeRule.onRoot().captureRoboImage(roborazziOptions = options)
    }

    private fun device(
        id: String,
        name: String,
        role: DeviceRole = DeviceRole.READER,
        pairedAt: Long = 1_767_225_600_000L,
        lastSeenAt: Long? = null,
        current: Boolean = false,
    ) = DeviceUi(
        deviceId = id,
        credentialId = "cred-$id",
        name = name,
        role = role,
        pairedAtEpochMs = pairedAt,
        lastSeenAtEpochMs = lastSeenAt,
        current = current,
        revoked = false,
    )

    @Test
    fun devices_populated() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                devices = listOf(
                    device(
                        "dev-1", "Pixel 8",
                        role = DeviceRole.CONTROLLER,
                        lastSeenAt = 1_772_445_600_000L,
                        current = true,
                    ),
                    device("dev-2", "Kitchen tablet", lastSeenAt = 1_772_532_000_000L),
                    device("dev-3", "Laptop browser", lastSeenAt = 1_772_359_200_000L),
                ),
            ),
        )
    }

    @Test
    fun devices_empty() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
            ),
        )
    }

    @Test
    fun devices_error() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                status = "Device credential was not found",
                statusIsError = true,
                devices = listOf(
                    device(
                        "dev-1", "Pixel 8",
                        role = DeviceRole.CONTROLLER,
                        lastSeenAt = 1_772_445_600_000L,
                        current = true,
                    ),
                ),
            ),
        )
    }

    @Test
    fun devices_invitation() {
        // A deterministic 21-module pattern standing in for the relay's QR.
        val dark = List(21 * 21) { index ->
            val column = index % 21
            val row = index / 21
            (column < 7 && row < 7) || (column > 13 && row < 7) ||
                (column < 7 && row > 13) || (column * row % 5 == 0)
        }
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                status = "Invitation created. Share the one-use link below before it expires.",
                invitation = InvitationUi(
                    link = "lerdr://pair#setup=AAAA&invite=inv_0123456789abcdef" +
                        "&invite_version=2&invite_expires=1893456000000" +
                        "&label=workstation&relay=ws%3A%2F%2F192.168.1.5%3A7474",
                    deviceName = "Kitchen tablet",
                    role = DeviceRole.READER,
                    expiresAtEpochMs = 1_893_456_000_000L,
                    qr = QrBitmapUi(size = 21, darkModules = dark),
                ),
                devices = listOf(
                    device(
                        "dev-1", "Pixel 8",
                        role = DeviceRole.CONTROLLER,
                        lastSeenAt = 1_772_445_600_000L,
                        current = true,
                    ),
                ),
            ),
        )
    }

    @Test
    fun devices_update_available() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                updateSupported = true,
                update = UpdateState(
                    state = "available",
                    currentVersion = "1.3.2",
                    availableVersion = "1.4.0",
                    availableRevision = "abc1234def5678",
                    targetVersion = "1.4.0",
                    targetRevision = "abc1234def5678",
                    canInstall = true,
                ),
                devices = listOf(
                    device(
                        "dev-1", "Pixel 8",
                        role = DeviceRole.CONTROLLER,
                        lastSeenAt = 1_772_445_600_000L,
                        current = true,
                    ),
                ),
            ),
        )
    }

    @Test
    fun devices_update_failed() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                updateSupported = true,
                update = UpdateState(
                    state = "failed",
                    error = "signature verification failed",
                ),
            ),
        )
    }

    @Test
    fun devices_update_manual_bootstrap() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                fetched = true,
                currentDeviceId = "dev-1",
            ),
        )
    }

    @Test
    fun devices_herdr_warnings() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                fetched = true,
                currentDeviceId = "dev-1",
                herdrStatus = HerdrStatus(
                    installedClientVersion = "0.9.0",
                    serverVersion = "0.8.2",
                    serverProtocol = 3,
                    serverProtocolKnown = true,
                    endpointProtocolGeneration = 7,
                    features = mapOf(
                        "pane.read" to HerdrFeatureStatus(
                            state = "supported",
                        ),
                        "workspace.move_block" to HerdrFeatureStatus(
                            state = "unsupported",
                            reason = "method_not_supported",
                        ),
                        "tab.move" to HerdrFeatureStatus(
                            state = "degraded",
                            reason = "reconnect_required",
                        ),
                    ),
                ),
            ),
        )
    }

    @Test
    fun devices_loading() {
        content(
            DevicesUiState(
                relayLabel = "workstation",
                connected = true,
                canAdminister = true,
                canInvite = true,
                loading = true,
                refreshing = true,
                currentDeviceId = "dev-1",
            ),
        )
    }
}
