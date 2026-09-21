package com.lerdr.core.testing

import com.google.common.truth.Truth.assertThat
import kotlin.io.path.exists
import org.junit.Assert.assertThrows
import org.junit.Test

class FixturesTest {
    @Test
    fun resolvesFixturesDirectory() {
        assertThat(Fixtures.dir().resolve("README.md").exists()).isTrue()
    }

    @Test
    fun loadsSuiteByName() {
        val suite = Fixtures.named("crypto.frames.json")
        assertThat(suite.formatVersion).isEqualTo(1)
        assertThat(suite.name).isEqualTo("crypto.frames.json")
        assertThat(suite.source.string("repo")).isEqualTo("IGUNUBLUE/lerdr")
        assertThat(suite.source.string("commit")).hasLength(40)
        assertThat(suite.vectors).hasSize(11)
    }

    @Test
    fun loadsSuiteByPath() {
        val suite = Fixtures.load("crypto/crypto.failures.json")
        assertThat(suite.name).isEqualTo("crypto.failures")
        assertThat(suite.vectors).hasSize(17)
    }

    @Test
    fun typedAccessors() {
        val vector = Fixtures.named("crypto.frames.json").vector("c2s-seq0-empty")
        assertThat(vector.string("direction")).isEqualTo("c2s")
        assertThat(vector.long("seq")).isEqualTo(0L)
        assertThat(vector.b64("plaintext_b64")).isEmpty()
        assertThat(vector.b64("nonce_b64")).hasLength(12)
        assertThat(vector.b64("nonce_b64")).isEqualTo(ByteArray(12))
    }

    @Test
    fun hexAccessor() {
        val vector = Fixtures.named("crypto.handshake.credential").vector("credential-basic")
        val binding = vector.hexBytes("binding_hex")
        assertThat(String(binding, Charsets.US_ASCII))
            .startsWith("herdr-e2ee-v2 auth\u0000credential\u0000")
    }

    @Test
    fun vectorLookupFailsClearly() {
        val suite = Fixtures.named("crypto.frames.json")
        val failure = assertThrows(IllegalStateException::class.java) { suite.vector("nope") }
        assertThat(failure).hasMessageThat().contains("nope")
    }

    @Test
    fun base64UrlRoundTripAndStrictness() {
        val bytes = byteArrayOf(0, 1, 2, 0x7f, -1, -2)
        val encoded = bytes.encodeBase64Url()
        assertThat(encoded).doesNotContain("=")
        assertThat(encoded).doesNotContain("+")
        assertThat(encoded).doesNotContain("/")
        assertThat(decodeBase64Url(encoded)).isEqualTo(bytes)
        // RawURLEncoding rejects padding; java.util's decoder alone would accept it.
        assertThrows(IllegalArgumentException::class.java) { decodeBase64Url("aGk=") }
        assertThrows(IllegalArgumentException::class.java) { decodeBase64Url("!!!") }
    }

    @Test
    fun missingFieldFailsClearly() {
        val vector = Fixtures.named("crypto.failures").vector("replay-c2s-second-delivery")
        assertThrows(IllegalArgumentException::class.java) { vector.string("no_such_field") }
    }
}
