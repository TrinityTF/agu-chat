package lol.agu.chat

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The 32-byte key the Rust side encrypts logins with (secrets.bin, as on desktop). It is stored wrapped by an
 * AES key that lives in the Android Keystore and never leaves it, so a copy of the app's files alone opens nothing.
 */
object KeyVault {
    private const val ALIAS = "agu-chat-secrets"
    private const val FILE = "secrets.key"

    private fun wrapper(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return gen.generateKey()
    }

    @Synchronized
    fun secretsKey(context: Context): ByteArray {
        val f = File(context.filesDir, FILE)
        val w = wrapper()
        if (f.exists()) {
            runCatching {
                val all = f.readBytes()
                val c = Cipher.getInstance("AES/GCM/NoPadding")
                c.init(Cipher.DECRYPT_MODE, w, GCMParameterSpec(128, all.copyOfRange(0, 12)))
                return c.doFinal(all.copyOfRange(12, all.size))
            }
            // the Keystore key is gone (restore to a new phone, cleared credentials): start over, logins are lost
        }
        val key = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val c = Cipher.getInstance("AES/GCM/NoPadding")
        c.init(Cipher.ENCRYPT_MODE, w)
        val tmp = File(context.filesDir, "$FILE.tmp")
        tmp.writeBytes(c.iv + c.doFinal(key))
        tmp.renameTo(f)
        return key
    }
}
