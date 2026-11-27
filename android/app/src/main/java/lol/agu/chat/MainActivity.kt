package lol.agu.chat

import android.graphics.Color
import android.content.pm.ApplicationInfo
import android.os.Bundle
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.jsonObject
import androidx.activity.SystemBarStyle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import lol.agu.chat.ui.AguTheme
import lol.agu.chat.ui.ChatViewModel
import lol.agu.chat.ui.MainScreen

class MainActivity : ComponentActivity() {
    private val vm: ChatViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(Color.TRANSPARENT),
        )
        setContent { AguTheme { MainScreen(vm) } }
        devFrames()
    }

    /**
     * Debuggable builds: `adb shell am start -n lol.agu.chat/.MainActivity --es devFrames <file in filesDir>` feeds the
     * frames in that JSON file (`[{sk, frame}]`) to their open chats, to check real rows without a busy channel.
     */
    private fun devFrames() {
        if (applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE == 0) return
        val name = intent?.getStringExtra("devFrames") ?: return
        lifecycleScope.launch {
            delay(1500)
            val items = runCatching { Core.json.parseToJsonElement(java.io.File(filesDir, name).readText()).jsonArray }.getOrNull() ?: return@launch
            for (it in items) {
                // `{call, args}` runs any call (e.g. settings.set with highlight rules, dev.me); `{sk, frame}` feeds a frame
                val o = it.jsonObject
                o["wait"]?.jsonPrimitive?.contentOrNull?.toLongOrNull()?.let { delay(it) }
                val call = o["call"]?.jsonPrimitive?.contentOrNull
                if (call == null && o["sk"] == null) continue
                runCatching { if (call != null) Core.call(call, o["args"]?.jsonObject ?: JsonObject(emptyMap())) else Core.call("dev.frame", o) }
                    .onFailure { e -> android.util.Log.w("devFrames", "${call ?: "frame"}: ${e.message}") }
                delay(120)
            }
        }
    }

    override fun onResume() {
        super.onResume()
        vm.setActive(true)
    }

    override fun onPause() {
        vm.setActive(false)
        super.onPause()
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        devFrames()
    }
}
