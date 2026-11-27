package lol.agu.chat.ui

import android.annotation.SuppressLint
import android.graphics.Bitmap
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import kotlinx.coroutines.delay

/** What a platform's sign-in has to produce: the cookies to read, where, and when the sign-in counts as done. */
private class LoginSpec(val title: String, val cookieUrl: String, val names: Set<String>, val complete: (Map<String, String>) -> Boolean)

private val YOUTUBE_KEPT = setOf(
    "SID", "HSID", "SSID", "APISID", "SAPISID", "__Secure-1PSID", "__Secure-3PSID", "__Secure-1PAPISID", "__Secure-3PAPISID",
    "__Secure-1PSIDTS", "__Secure-3PSIDTS", "SIDCC", "__Secure-1PSIDCC", "__Secure-3PSIDCC", "LOGIN_INFO",
)

// The same rules as the desktop sign-in windows (youtube_login.rs, x_login.rs, twitch_login.rs); core checks them again.
private val specs = mapOf(
    "twitch" to LoginSpec("Log in to Twitch", "https://www.twitch.tv", setOf("auth-token")) { !it["auth-token"].isNullOrEmpty() },
    "x" to LoginSpec("Log in to X", "https://x.com", setOf("auth_token", "ct0", "twid")) { !it["auth_token"].isNullOrEmpty() && !it["ct0"].isNullOrEmpty() },
    "youtube" to LoginSpec("Log in to YouTube", "https://www.youtube.com", YOUTUBE_KEPT) {
        (!it["SAPISID"].isNullOrEmpty() || !it["__Secure-3PAPISID"].isNullOrEmpty()) && !it["LOGIN_INFO"].isNullOrEmpty()
    },
)

/** The sign-in WebView's cookie jar (nothing else in the app uses a WebView). */
object LoginCookies {
    fun clear() {
        runCatching {
            val cm = CookieManager.getInstance()
            cm.removeAllCookies(null)
            cm.flush()
        }
    }

    /** The named cookies the site has set; values are only ever handed to the core, never logged. */
    fun read(url: String, names: Set<String>): Map<String, String> {
        val raw = runCatching { CookieManager.getInstance().getCookie(url) }.getOrNull() ?: return emptyMap()
        val out = HashMap<String, String>()
        for (part in raw.split(';')) {
            val i = part.indexOf('=')
            if (i <= 0) continue
            val k = part.substring(0, i).trim()
            if (k in names && k !in out) out[k] = part.substring(i + 1).trim()
        }
        return out
    }
}

/**
 * Full-screen sign-in: the site's own login page in a WebView (its default user agent), so the password goes to the
 * site and never to Agu Chat. When the sign-in cookies show up they go to the core and the screen closes; back or
 * the close button is a cancel.
 */
@SuppressLint("SetJavaScriptEnabled")
@Composable
fun LoginWebScreen(request: LoginRequest, vm: ChatViewModel) {
    val spec = specs[request.platform] ?: return
    val ctx = LocalContext.current
    var progress by remember { mutableStateOf(0) }
    var title by remember { mutableStateOf(spec.title) }
    // Twitch: the cookie came back, the screen waits for the authorise page
    var handedOver by remember { mutableStateOf(false) }
    val web = remember {
        WebView(ctx).apply {
            layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
            settings.javaScriptEnabled = true
            settings.domStorageEnabled = true
            settings.mediaPlaybackRequiresUserGesture = true
            settings.cacheMode = WebSettings.LOAD_DEFAULT
            val wv = this
            CookieManager.getInstance().apply {
                setAcceptCookie(true)
                setAcceptThirdPartyCookies(wv, true)
            }
            webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, r: WebResourceRequest): Boolean {
                    // sign-in pages stay in the WebView; app links (intent:, market:, twitch:) go nowhere
                    val s = r.url.scheme
                    return s != "https" && s != "http" && s != "about"
                }

                override fun onPageStarted(view: WebView, url: String?, favicon: Bitmap?) { progress = 10 }
                override fun onPageFinished(view: WebView, url: String?) { progress = 100 }
            }
            webChromeClient = object : android.webkit.WebChromeClient() {
                override fun onProgressChanged(view: WebView, newProgress: Int) { progress = newProgress }
            }
        }
    }
    androidx.compose.runtime.DisposableEffect(Unit) {
        onDispose {
            web.stopLoading()
            web.loadUrl("about:blank")
            web.destroy()
        }
    }
    val cancel = { vm.loginAnswer(request) }
    BackHandler { if (!request.show && !handedOver && web.canGoBack()) web.goBack() else cancel() }

    LaunchedEffect(request.id) {
        handedOver = false
        web.loadUrl(request.url)
        if (request.show) return@LaunchedEffect
        // the same rule as the desktop windows: poll the cookie jar until the sign-in cookies are there
        while (true) {
            delay(500)
            val got = LoginCookies.read(spec.cookieUrl, spec.names)
            if (spec.complete(got)) {
                handedOver = true
                if (request.platform == "twitch") web.loadUrl("about:blank") // twitch.tv's front page autoplays a stream
                // Twitch stays up for the authorise page core sends next
                vm.loginAnswer(request, cookies = got.toList(), keepOpen = request.platform == "twitch")
                return@LaunchedEffect
            }
        }
    }

    Column(Modifier.fillMaxSize().background(AguColors.Bg).statusBarsPadding().navigationBarsPadding().imePadding()) {
        Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
            Text(
                "×", color = AguColors.Text, fontSize = 26.sp,
                modifier = Modifier.clip(CircleShape).clickable(onClick = { cancel() }).padding(horizontal = 16.dp, vertical = 2.dp),
            )
            Text(title, color = AguColors.Text, fontSize = 18.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
        if (progress in 1..99) LinearProgressIndicator(progress = { progress / 100f }, modifier = Modifier.fillMaxWidth(), color = AguColors.Accent)
        Box(Modifier.weight(1f).fillMaxWidth()) {
            AndroidView(factory = { web }, modifier = Modifier.fillMaxSize())
            if (handedOver && !request.show) {
                Box(Modifier.fillMaxSize().background(AguColors.Bg), contentAlignment = Alignment.Center) {
                    Text("Signing in…", color = AguColors.Muted, fontSize = 15.sp)
                }
            }
        }
    }
}
