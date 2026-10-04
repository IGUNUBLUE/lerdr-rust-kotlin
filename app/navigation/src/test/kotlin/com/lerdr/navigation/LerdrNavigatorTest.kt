package com.lerdr.navigation

import androidx.navigation3.runtime.NavBackStack
import com.google.common.truth.Truth.assertThat
import org.junit.Test

class LerdrNavigatorTest {

    private fun navigator(vararg elements: LerdrKey) =
        LerdrNavigator(NavBackStack(*elements))

    @Test
    fun backFromTopLevelTabLandsOnHome() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.goBack()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun visitedTabsDoNotAccumulateOnBack() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Computers)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.goBack()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun reselectingCurrentTabIsANoOp() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.navigateTopLevel(LerdrKey.Settings)
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Settings)
    }

    @Test
    fun topLevelNavPopsPushedEntriesBackToRoot() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.navigate(LerdrKey.RelayDetail("relay-1"))
        nav.navigateTopLevel(LerdrKey.Activity)
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Activity)
    }

    @Test
    fun homeTabPopsEverythingAboveIt() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.navigate(LerdrKey.RelayDetail("relay-1"))
        nav.navigateTopLevel(LerdrKey.Home)
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun pushedEntryPopsBackToItsTopLevelParent() {
        val nav = navigator(LerdrKey.Home)
        nav.navigateTopLevel(LerdrKey.Settings)
        nav.navigate(LerdrKey.RelayDetail("relay-1"))
        nav.goBack()
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Settings)
    }

    @Test
    fun agentModesSwapInPlaceForTheSamePane() {
        val nav = navigator(LerdrKey.Home)
        nav.openAgent("w:p1")
        nav.openFeed("w:p1")
        nav.openFiles("w:p1")
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Files("w:p1"))
    }

    @Test
    fun agentModesStackAcrossPanes() {
        val nav = navigator(LerdrKey.Home)
        nav.openAgent("w:p1")
        nav.openAgent("w:p2")
        nav.goBack()
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Terminal("w:p1"))
    }

    @Test
    fun replacementPreservesModesAndRemovesClosedPaneBackTargets() {
        val nav = navigator(
            LerdrKey.Home,
            LerdrKey.AgentFeed("r1::old"),
            LerdrKey.Terminal("r1::other"),
            LerdrKey.Files("r1::old"),
        )
        nav.replaceAgent("r1::old", "r1::new")
        assertThat(nav.backStack.toList()).containsExactly(
            LerdrKey.Home,
            LerdrKey.AgentFeed("r1::new"),
            LerdrKey.Terminal("r1::other"),
            LerdrKey.Files("r1::new"),
        ).inOrder()
        nav.goBack()
        nav.goBack()
        assertThat(nav.backStack.last()).isEqualTo(LerdrKey.AgentFeed("r1::new"))
    }

    @Test
    fun matchingTopLevelDeepLinkDoesNotAddAnotherBackStep() {
        val nav = navigator(LerdrKey.Home, LerdrKey.Settings)
        nav.navigate(LerdrDeepLinks.match("lerdr://settings")!!)
        nav.goBack()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun homeDeepLinkClearsTheCurrentTabAndItsDetails() {
        val nav = navigator(LerdrKey.Home, LerdrKey.Settings, LerdrKey.RelayDetail("r1"))
        nav.navigate(LerdrDeepLinks.match("lerdr://agents")!!)
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun matchingPaneDeepLinkReplacesTheCurrentSessionMode() {
        val nav = navigator(LerdrKey.Home, LerdrKey.Terminal("r1::%1"))
        val link = LerdrDeepLinks.match("lerdr://agent?pane_id=r1%3A%3A%251")!!
        nav.navigate(link)
        nav.navigate(link)
        assertThat(nav.backStack.last()).isEqualTo(LerdrKey.AgentFeed("r1::%1"))
        nav.goBack()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun warmPairingLinkReplacesTheOpenPairingForm() {
        val nav = navigator(LerdrKey.Home, LerdrKey.Computers, LerdrKey.Pairing())
        val first = LerdrDeepLinks.match("lerdr://pair?setup=first&label=First")!!
        val second = LerdrDeepLinks.match("lerdr://pair?setup=second&label=Second")!!
        nav.navigate(first)
        nav.navigate(second)
        nav.navigate(second)
        assertThat(nav.backStack.last()).isEqualTo(second)
        nav.goBack()
        assertThat(nav.backStack.toList())
            .containsExactly(LerdrKey.Home, LerdrKey.Computers).inOrder()
    }

    @Test
    fun pairingCompletionReturnsToHomeRatherThanTheLaunchingTab() {
        val nav = navigator(LerdrKey.Home, LerdrKey.Computers, LerdrKey.Pairing())
        nav.onPairingComplete()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }

    @Test
    fun backAtTheRootDoesNotLeaveAnEmptyGraph() {
        val nav = navigator(LerdrKey.Home)
        nav.goBack()
        assertThat(nav.backStack.toList()).containsExactly(LerdrKey.Home)
    }
}
