package com.phantom.demo

import android.opengl.GLSurfaceView
import android.os.BatteryManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Switch
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

class MainActivity : AppCompatActivity() {

    private lateinit var glSurfaceView: GLSurfaceView
    private lateinit var renderer: CubeRenderer
    private lateinit var tvFps: TextView
    private lateinit var tvBattery: TextView
    private lateinit var tvMode: TextView
    private lateinit var switchMode: Switch

    private val uiHandler = Handler(Looper.getMainLooper())
    private val uiUpdater = object : Runnable {
        override fun run() {
            updateUI()
            uiHandler.postDelayed(this, 500)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        glSurfaceView = findViewById(R.id.gl_surface)
        tvFps = findViewById(R.id.tv_fps)
        tvBattery = findViewById(R.id.tv_battery)
        tvMode = findViewById(R.id.tv_mode)
        switchMode = findViewById(R.id.switch_mode)

        // Configure OpenGL ES 3.1
        glSurfaceView.setEGLContextClientVersion(3)

        renderer = CubeRenderer(this)
        glSurfaceView.setRenderer(renderer)
        glSurfaceView.renderMode = GLSurfaceView.RENDERMODE_CONTINUOUSLY

        switchMode.setOnCheckedChangeListener { _, isChecked ->
            renderer.phantomCoreEnabled = isChecked
            tvMode.text = if (isChecked) "⚡ PHANTOM CORE" else "📱 LOCAL RENDER"
            tvMode.setTextColor(if (isChecked) 0xFF00FF88.toInt() else 0xFFFF7B72.toInt())
        }

        uiHandler.postDelayed(uiUpdater, 500)
    }

    private fun updateUI() {
        tvFps.text = "FPS: ${renderer.currentFps}"

        val batteryManager = getSystemService(BATTERY_SERVICE) as BatteryManager
        val level = batteryManager.getIntProperty(BatteryManager.BATTERY_PROPERTY_CAPACITY)
        val currentNow = batteryManager.getIntProperty(BatteryManager.BATTERY_PROPERTY_CURRENT_NOW)
        tvBattery.text = "🔋 $level%  |  ${currentNow / 1000} mA"
    }

    override fun onResume() {
        super.onResume()
        glSurfaceView.onResume()
    }

    override fun onPause() {
        super.onPause()
        glSurfaceView.onPause()
        uiHandler.removeCallbacks(uiUpdater)
    }
}
