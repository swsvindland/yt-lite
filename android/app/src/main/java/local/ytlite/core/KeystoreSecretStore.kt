package local.ytlite.core

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import uniffi.yt_lite_ffi.FfiException
import uniffi.yt_lite_ffi.SecretStore

/**
 * Where the Rust core keeps the Google refresh token: values encrypted with
 * an AES key that never leaves the Android Keystore. The key doesn't require
 * an unlocked phone, so Android Auto can refresh Subscriptions while it's locked.
 */
class KeystoreSecretStore(context: Context) : SecretStore {
    private val prefs = context.getSharedPreferences("secrets", Context.MODE_PRIVATE)

    @Synchronized
    override fun get(key: String): String? = guarded {
        val stored = prefs.getString(key, null) ?: return@guarded null
        val sealed = Base64.decode(stored, Base64.NO_WRAP)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, sealed, 0, IV_BYTES))
        String(cipher.doFinal(sealed, IV_BYTES, sealed.size - IV_BYTES), Charsets.UTF_8)
    }

    @Synchronized
    override fun set(key: String, value: String) = guarded {
        val cipher = Cipher.getInstance(TRANSFORMATION).apply { init(Cipher.ENCRYPT_MODE, key()) }
        val sealed = cipher.iv + cipher.doFinal(value.toByteArray(Charsets.UTF_8))
        check(prefs.edit().putString(key, Base64.encodeToString(sealed, Base64.NO_WRAP)).commit()) {
            "couldn't write"
        }
    }

    @Synchronized
    override fun delete(key: String) = guarded {
        check(prefs.edit().remove(key).commit()) { "couldn't write" }
    }

    private fun key(): SecretKey {
        val keyStore = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (keyStore.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val spec = KeyGenParameterSpec.Builder(
            ALIAS,
            KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
        )
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
            .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256)
            .build()
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
            .apply { init(spec) }
            .generateKey()
    }

    /** The core expects failures as FfiException. */
    private inline fun <T> guarded(block: () -> T): T =
        try {
            block()
        } catch (e: Exception) {
            throw FfiException.Failed("secure storage: ${e.message ?: e.javaClass.simpleName}")
        }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "yt-lite-secrets"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
    }
}
