package com.lerdr.app.home

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import org.junit.Test

/**
 * `AttentionCardUi` quick-answer eligibility — the oracle's rule: a lone
 * `single_select` question without a free-text Other answers inline;
 * anything else takes the session's full form.
 */
class HomeUiStateTest {

    private fun card(interaction: Interaction?, kind: AttentionKind = AttentionKind.QUESTION) =
        AttentionCardUi(
            paneId = "sd::%1",
            relayId = "sd",
            agentLabel = "claude · lerdr",
            kind = kind,
            metaLabel = "question · 2 options",
            prompt = "Pick one",
            interaction = interaction,
        )

    private fun interaction(
        kind: String = "single_select",
        questionTotal: Int = 1,
        otherHidden: Boolean = true,
        options: List<Option> = listOf(
            Option(index = 0, label = "store"),
            Option(index = 2, label = "session"),
        ),
    ) = Interaction(
        id = "q1",
        kind = kind,
        question = "Pick one",
        options = options,
        other = Other(hidden = otherHidden),
        questionTotal = questionTotal,
    )

    @Test
    fun `lone single_select without Other is quick-answerable`() {
        val card = card(interaction())
        assertThat(card.quickOptions.map { it.index }).containsExactly(0, 2).inOrder()
        assertThat(card.chooseLabel).isNull()
    }

    @Test
    fun `multi_select falls back to the full form`() {
        val card = card(interaction(kind = "multi_select"))
        assertThat(card.quickOptions).isEmpty()
        assertThat(card.chooseLabel).isEqualTo("Choose options (2)")
    }

    @Test
    fun `a follow-up question in a sequence falls back to the full form`() {
        val card = card(interaction(questionTotal = 3))
        assertThat(card.quickOptions).isEmpty()
        assertThat(card.chooseLabel).isEqualTo("Choose answer (2)")
    }

    @Test
    fun `a visible Other option falls back to the full form`() {
        val card = card(interaction(otherHidden = false))
        assertThat(card.quickOptions).isEmpty()
        assertThat(card.chooseLabel).isEqualTo("Choose answer (2)")
    }

    @Test
    fun `an unrecognized interaction kind still offers the full form`() {
        val card = card(interaction(kind = "choice"))
        assertThat(card.quickOptions).isEmpty()
        assertThat(card.chooseLabel).isEqualTo("Choose answer (2)")
    }

    @Test
    fun `approvals and missing interactions expose neither affordance`() {
        assertThat(card(null).quickOptions).isEmpty()
        assertThat(card(null).chooseLabel).isNull()
        assertThat(card(interaction(), kind = AttentionKind.APPROVAL).quickOptions).isEmpty()
        assertThat(card(interaction(), kind = AttentionKind.APPROVAL).chooseLabel).isNull()
    }
}
