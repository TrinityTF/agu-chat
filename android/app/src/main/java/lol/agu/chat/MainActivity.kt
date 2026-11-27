package lol.agu.chat

import android.graphics.Color
import android.content.pm.ApplicationInfo
import android.os.Bundle
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.json.jsonArray
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
                runCatching { Core.call("dev.frame", it.jsonObject) }
                delay(120)
            }
        }
    }

    override fun onNewIntent(intent: android.content.Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        devFrames()
    }
}
