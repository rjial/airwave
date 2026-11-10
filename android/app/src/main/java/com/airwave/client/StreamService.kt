package com.airwave.client

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/** Keeps sound going while the screen is off, with a small notification. */
class StreamService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val playing = Airwave.isPlaying
        val sending = Airwave.isSending
        if (!playing && !sending) {
            stopSelf()
            return START_NOT_STICKY
        }
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "Streaming", NotificationManager.IMPORTANCE_LOW)
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
        )
        val text = when {
            playing && sending -> "Playing and sending sound"
            playing -> "Playing sound from your computer"
            else -> "Sending this phone's microphone"
        }
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_airwave)
            .setContentTitle("Airwave")
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .build()
        var type = 0
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            if (playing) type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
            if (sending && Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                type = type or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            }
        }
        try {
            ServiceCompat.startForeground(this, 1, notification, type)
        } catch (_: Exception) {
            // The app keeps working while open even if the system refuses.
            stopSelf()
        }
        return START_NOT_STICKY
    }

    companion object {
        private const val CHANNEL = "airwave"
        private val main = Handler(Looper.getMainLooper())

        /** Start or stop the service to match what is happening now. */
        fun update(context: Context) {
            val app = context.applicationContext
            main.post {
                val intent = Intent(app, StreamService::class.java)
                if (Airwave.isPlaying || Airwave.isSending) {
                    runCatching { ContextCompat.startForegroundService(app, intent) }
                } else {
                    app.stopService(intent)
                }
            }
        }
    }
}
