package com.lerdr.app.security

import android.content.Context
import android.content.ContextWrapper
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.tooling.preview.PreviewLightDark
import androidx.compose.ui.unit.dp
import androidx.fragment.app.FragmentActivity
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import com.lerdr.app.R
import com.lerdr.core.designsystem.theme.LerdrTheme
import dagger.hilt.android.EntryPointAccessors

/**
 * Whole-app lock gate — while [locked] the children are not composed at
 * all (the oracle's unlock screen "covers the page", docs/security.md)
 * and a minimal brand surface takes over. Entering the locked state
 * fires exactly one [onUnlockRequest]; a dismissal or failure leaves the
 * surface up and the button retries. Unlocking drops the branch and the
 * content composes fresh.
 */
@Composable
fun LockGate(
    locked: Boolean,
    onUnlockRequest: () -> Unit,
    content: @Composable () -> Unit,
) {
    if (!locked) {
        content()
        return
    }
    // Inside the `locked` branch this fires once per entry into the
    // locked state — the automatic prompt on (re)start and on toggle-on.
    LaunchedEffect(Unit) { onUnlockRequest() }
    LockedSurface(onUnlock = onUnlockRequest)
}

/**
 * Self-wiring variant for the composition root — resolves the lock
 * state, preferences, and prompt helper through [SecurityEntryPoint] so
 * `MainActivity` wraps the nav display with a single `LockGate { … }`.
 *
 * `BiometricPrompt` needs a [FragmentActivity] to attach to; if the host
 * context is not one the gate opens rather than stranding the user —
 * the same fail-open policy the helper applies to devices with nothing
 * to verify against.
 */
@Composable
fun LockGate(content: @Composable () -> Unit) {
    val context = LocalContext.current
    val appContext = context.applicationContext
    val entryPoint = remember(appContext) {
        EntryPointAccessors.fromApplication(appContext, SecurityEntryPoint::class.java)
    }
    val viewModel: LockViewModel = viewModel {
        LockViewModel(entryPoint.lockState())
    }
    val promptHelper = remember(entryPoint) { entryPoint.biometricPromptHelper() }
    val locked by viewModel.locked.collectAsStateWithLifecycle()

    LockGate(
        locked = locked,
        onUnlockRequest = {
            val host = context.findFragmentActivity()
            if (host == null) {
                viewModel.unlock()
            } else {
                promptHelper.authenticate(host) { unlocked ->
                    if (unlocked) viewModel.unlock()
                }
            }
        },
        content = content,
    )
}

@Composable
private fun LockedSurface(onUnlock: () -> Unit) {
    val spacing = LerdrTheme.spacing
    Surface(modifier = Modifier.fillMaxSize()) {
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(spacing.extraLarge),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            BrandMark()
            Spacer(Modifier.height(spacing.large))
            Text("Lerdr", style = MaterialTheme.typography.headlineMedium)
            Spacer(Modifier.height(spacing.extraSmall))
            Text(
                "Locked — verify to continue",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(spacing.large))
            Button(onClick = onUnlock) {
                Icon(Icons.Default.Fingerprint, contentDescription = null)
                Spacer(Modifier.width(spacing.small))
                Text("Unlock")
            }
        }
    }
}

/**
 * The launcher iguana re-composed in-theme: the adaptive-icon foreground
 * vector over the same indigo→violet gradient `ic_launcher_background`
 * draws — painted in Compose because `aapt:attr` vector gradients do not
 * survive `painterResource` under previews and Robolectric.
 */
@Composable
private fun BrandMark(modifier: Modifier = Modifier) {
    Box(
        modifier = modifier
            .size(96.dp)
            .clip(MaterialTheme.shapes.extraLarge)
            .background(
                Brush.linearGradient(listOf(Color(0xFF6366F1), Color(0xFFA855F7))),
            ),
    ) {
        Image(
            painter = painterResource(R.drawable.ic_launcher_foreground),
            contentDescription = null,
            modifier = Modifier.fillMaxSize(),
        )
    }
}

private tailrec fun Context.findFragmentActivity(): FragmentActivity? = when (this) {
    is FragmentActivity -> this
    is ContextWrapper -> baseContext.findFragmentActivity()
    else -> null
}

@PreviewLightDark
@Composable
private fun LockedSurfacePreview() {
    LerdrTheme {
        LockedSurface(onUnlock = {})
    }
}
