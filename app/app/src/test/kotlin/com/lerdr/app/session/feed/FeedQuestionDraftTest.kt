package com.lerdr.app.session.feed

import com.google.common.truth.Truth.assertThat
import lerdr.core.model.Interaction
import lerdr.core.model.Option
import lerdr.core.model.Other
import org.junit.Test

/**
 * Pure-function ports of the oracle's `frontend/src/lib/questions.ts` —
 * draft creation, submit gating, option/other updates, restore rules.
 */
class FeedQuestionDraftTest {

    private fun single(
        selected: List<Int> = emptyList(),
        other: Other = Other(),
        canGoBack: Boolean = false,
        canChat: Boolean = false,
        index: Int = 1,
        total: Int = 1,
        submitLabel: String = "Submit",
    ) = Interaction(
        id = "q1",
        kind = "single_select",
        question = "Pick one?",
        options = listOf(
            Option(index = 0, label = "Alpha", selected = 0 in selected),
            Option(index = 1, label = "Beta", selected = 1 in selected),
        ),
        other = other,
        submitLabel = submitLabel,
        canChat = canChat,
        canGoBack = canGoBack,
        questionIndex = index,
        questionTotal = total,
    )

    private fun multi(other: Other = Other()) = Interaction(
        id = "q2",
        kind = "multi_select",
        question = "Pick many?",
        options = listOf(
            Option(index = 0, label = "Alpha"),
            Option(index = 1, label = "Beta"),
        ),
        other = other,
    )

    @Test
    fun `createQuestionDraft mirrors the interaction's selected flags`() {
        val draft = createQuestionDraft(
            single(
                selected = listOf(1),
                other = Other(selected = true, text = "note"),
            ),
        )
        assertThat(draft.selected).containsExactly(1)
        assertThat(draft.otherSelected).isTrue()
        assertThat(draft.otherText).isEqualTo("note")
    }

    @Test
    fun `single-select submit needs one option or a valid Other`() {
        val interaction = single()
        assertThat(questionSubmitAllowed(interaction, QuestionDraft())).isFalse()
        assertThat(
            questionSubmitAllowed(interaction, QuestionDraft(selected = setOf(0))),
        ).isTrue()
        // Other selected but empty text → not allowed unless allow_empty.
        assertThat(
            questionSubmitAllowed(interaction, QuestionDraft(otherSelected = true)),
        ).isFalse()
        assertThat(
            questionSubmitAllowed(
                single(other = Other(allowEmpty = true)),
                QuestionDraft(otherSelected = true),
            ),
        ).isTrue()
        assertThat(
            questionSubmitAllowed(
                interaction,
                QuestionDraft(otherSelected = true, otherText = "custom"),
            ),
        ).isTrue()
    }

    @Test
    fun `multi-select always submits`() {
        assertThat(questionSubmitAllowed(multi(), QuestionDraft())).isTrue()
    }

    @Test
    fun `single option pick clears the Other slot`() {
        val interaction = single()
        val draft = QuestionDraft(otherSelected = true, otherText = "typed")
        val next = updateQuestionOption(interaction, draft, index = 1, checked = true)
        assertThat(next.selected).containsExactly(1)
        assertThat(next.otherSelected).isFalse()
        assertThat(next.otherText).isEmpty()
    }

    @Test
    fun `multi option toggle adds and removes`() {
        val interaction = multi()
        val on = updateQuestionOption(interaction, QuestionDraft(), 0, true)
        assertThat(on.selected).containsExactly(0)
        val off = updateQuestionOption(interaction, on, 0, false)
        assertThat(off.selected).isEmpty()
        // Other coexists with options on multi-select.
        val both = updateQuestionOther(
            interaction,
            on.copy(otherText = "extra"),
            selected = true,
        )
        assertThat(both.selected).containsExactly(0)
        assertThat(both.otherSelected).isTrue()
        assertThat(both.otherText).isEqualTo("extra")
    }

    @Test
    fun `selecting Other on single-select clears the options`() {
        val interaction = single()
        val draft = QuestionDraft(selected = setOf(0))
        val next = updateQuestionOther(interaction, draft, selected = true)
        assertThat(next.selected).isEmpty()
        assertThat(next.otherSelected).isTrue()
    }

    @Test
    fun `multi Other deselect wipes its text`() {
        val interaction = multi()
        val draft = QuestionDraft(otherSelected = true, otherText = "typed")
        val next = updateQuestionOther(interaction, draft, selected = false)
        assertThat(next.otherSelected).isFalse()
        assertThat(next.otherText).isEmpty()
    }

    @Test
    fun `typing in Other selects it`() {
        // single: always selects, even when clearing.
        val singleInteraction = single()
        assertThat(
            changeQuestionOtherText(singleInteraction, QuestionDraft(), "").otherSelected,
        ).isTrue()
        // multi: only while non-empty.
        val multiInteraction = multi()
        assertThat(
            changeQuestionOtherText(multiInteraction, QuestionDraft(), "x").otherSelected,
        ).isTrue()
        assertThat(
            changeQuestionOtherText(multiInteraction, QuestionDraft(), "").otherSelected,
        ).isFalse()
    }

    @Test
    fun `dirty draft restores when it still submits or the baseline cannot`() {
        val interaction = single()
        val good = QuestionDraft(selected = setOf(0))
        val bad = QuestionDraft()
        val incomingGood = createQuestionDraft(single(selected = listOf(1)))
        val incomingBad = createQuestionDraft(single())
        // cached submits → restore over a worse baseline
        assertThat(shouldRestoreQuestionDraft(interaction, good, incomingBad)).isTrue()
        // cached can't submit but incoming can't either → keep the user's work
        assertThat(shouldRestoreQuestionDraft(interaction, bad, incomingBad)).isTrue()
        // cached can't submit, incoming can → take the fresh baseline
        assertThat(shouldRestoreQuestionDraft(interaction, bad, incomingGood)).isFalse()
        assertThat(shouldRestoreQuestionDraft(interaction, null, incomingGood)).isFalse()
    }

    @Test
    fun `progress renders only inside the 1-based range`() {
        assertThat(questionProgress(single(index = 2, total = 3))).isEqualTo("Question 2 of 3")
        assertThat(questionProgress(single(index = 0, total = 3))).isEmpty()
        assertThat(questionProgress(single(index = 4, total = 3))).isEmpty()
        assertThat(questionProgress(single(index = 1, total = 0))).isEmpty()
    }
}
