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
}
