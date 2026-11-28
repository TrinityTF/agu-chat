package lol.agu.chat

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import java.lang.ref.WeakReference

/** Opens a login page in the browser. */
object Browser {
    /** The chat screen while it exists, so a login tab can open inside the app's own task. */
    @Volatile var activity: WeakReference<Activity>? = null

    /**
     * From the chat screen it is a Custom Tab in the app's task (the extra below is what the Custom Tabs library sets),
     * so when the login comes back to the app the tab above it is cleared; otherwise a plain browser window.
     */
    fun open(ctx: Context, url: String) {
        val a = (ctx as? Activity) ?: activity?.get()?.takeIf { !it.isFinishing && !it.isDestroyed }
        val i = Intent(Intent.ACTION_VIEW, Uri.parse(url))
        runCatching {
            if (a != null) {
                i.putExtras(Bundle().apply { putBinder("android.support.customtabs.extra.SESSION", null) })
                a.startActivity(i)
            } else {
                ctx.startActivity(i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
        }
    }
}
