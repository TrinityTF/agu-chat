package lol.agu.chat

import android.app.Activity
import android.content.Intent
import android.os.Bundle

/**
 * Where the Kick login's browser tab ends (`aguchat://kick-done`, sent by the app's own loopback page). It brings the
 * chat screen back with everything above it cleared, which closes that tab, and goes away. The link carries nothing:
 * the login itself already reached the app over the loopback, so any page opening it only brings the app forward.
 */
class KickReturnActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        startActivity(Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP))
        finish()
    }
}
