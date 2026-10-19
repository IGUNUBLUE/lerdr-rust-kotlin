package com.lerdr.app.notify

import android.app.Application
import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.google.common.truth.Truth.assertThat
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.GraphicsMode
import android.view.View
import android.widget.FrameLayout
import com.github.takahirom.roborazzi.RoborazziOptions
import com.github.takahirom.roborazzi.captureRoboImage

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = Application::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class LerdrNotifierTest {
    private val context = ApplicationProvider.getApplicationContext<Context>()
    private val manager = context.getSystemService(NotificationManager::class.java)

    @Test
    fun `agent notifications keep sensitive text out of the public version`() {
        for (channel in listOf(NotifyChannel.AGENT_ATTENTION, NotifyChannel.AGENT_ACTIVITY)) {
            LerdrNotifier(context).execute(listOf(NotificationCommand.Post(
                notificationId = 123,
                channel = channel,
                title = "Private project needs approval",
                body = "Run secret-command --token private-value?",
                deepLink = NotifyDeepLinks.agent("private-relay::pane"),
                inboxLines = listOf("Private workspace"),
            )))
            val posted = shadowOf(manager).getNotification(123)
            assertThat(posted.visibility).isEqualTo(Notification.VISIBILITY_PRIVATE)
            assertThat(posted.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
                .contains("private-value")
            val public = posted.publicVersion
            assertThat(public.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
                .isEqualTo("Lerdr")
            assertThat(public.extras.getCharSequence(Notification.EXTRA_TEXT).toString())
                .isEqualTo("Open Lerdr to view agent updates")
            assertThat(public.extras.getCharSequenceArray(Notification.EXTRA_TEXT_LINES)).isNull()
            assertThat(public.contentIntent).isEqualTo(posted.contentIntent)
        }
    }

    @Test
    fun `restored attention card is silent even when its former card was canceled`() {
        val notifier = LerdrNotifier(context)
        val command = NotificationCommand.Post(
            notificationId = 123,
            channel = NotifyChannel.AGENT_ATTENTION,
            title = "Agent needs approval",
            body = "Question",
            deepLink = NotifyDeepLinks.agent("r1::p1"),
            silent = true,
        )
        notifier.execute(listOf(NotificationCommand.Cancel(123), command))
        val posted = shadowOf(manager).getNotification(123)
        assertThat(posted.sound).isNull()
        assertThat(posted.vibrate).isNull()
        assertThat(posted.category).isEqualTo(Notification.CATEGORY_REMINDER)
        assertThat(posted.groupAlertBehavior).isEqualTo(Notification.GROUP_ALERT_SUMMARY)
    }

    @Test
    fun publicNotificationScreenshot() {
        LerdrNotifier(context).execute(listOf(NotificationCommand.Post(
            notificationId = 123,
            channel = NotifyChannel.AGENT_ATTENTION,
            title = "Private project needs approval",
            body = "Run secret-command --token private-value?",
            deepLink = NotifyDeepLinks.agent("r1::p1"),
        )))
        val public = shadowOf(manager).getNotification(123).publicVersion
        val remoteViews = Notification.Builder.recoverBuilder(context, public).createContentView()
        val controller = Robolectric.buildActivity(android.app.Activity::class.java)
        controller.get().setTheme(android.R.style.Theme_Material_Light_NoActionBar)
        val activity = controller.setup().get()
        val view = remoteViews.apply(activity, FrameLayout(activity))
        view.setBackgroundColor(android.graphics.Color.WHITE)
        activity.setContentView(view)
        val width = (393 * context.resources.displayMetrics.density).toInt()
        view.measure(
            View.MeasureSpec.makeMeasureSpec(width, View.MeasureSpec.EXACTLY),
            View.MeasureSpec.makeMeasureSpec(0, View.MeasureSpec.UNSPECIFIED),
        )
        view.layout(0, 0, view.measuredWidth, view.measuredHeight)
        view.captureRoboImage(roborazziOptions = RoborazziOptions(
            captureType = RoborazziOptions.CaptureType.Screenshot(),
        ))
        controller.pause().stop().destroy()
    }

    @Test
    fun `data sync timeout stops the service without restarting it`() {
        // get() attaches the service without onCreate's injected live stores.
        val service = Robolectric.buildService(RelaySyncService::class.java).get()
        service.onTimeout(7, android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        assertThat(shadowOf(service).isStoppedBySelf).isTrue()
    }
}
