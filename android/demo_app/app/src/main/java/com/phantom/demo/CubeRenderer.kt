package com.phantom.demo

import android.content.Context
import android.opengl.GLES31
import android.opengl.GLSurfaceView
import android.util.Log
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.FloatBuffer
import javax.microedition.khronos.egl.EGLConfig
import javax.microedition.khronos.opengles.GL10

class CubeRenderer(private val context: Context) : GLSurfaceView.Renderer {

    companion object {
        const val TAG = "CubeRenderer"
        const val TEX_WIDTH = 512
        const val TEX_HEIGHT = 512
    }

    var phantomCoreEnabled = false
    var currentFps = 0
        private set

    private var computeProgram = 0
    private var renderProgram = 0
    private var outputTexture = 0
    private var quadVAO = 0
    private var quadVBO = 0

    private var width = TEX_WIDTH
    private var height = TEX_HEIGHT
    private var frameCount = 0
    private var lastFpsTime = System.nanoTime()
    private var timeUniform = 0
    private var resolutionUniform = 0

    // Fullscreen quad vertices (position + texcoord)
    private val quadVertices = floatArrayOf(
        -1f, -1f, 0f, 0f,
         1f, -1f, 1f, 0f,
        -1f,  1f, 0f, 1f,
         1f, -1f, 1f, 0f,
         1f,  1f, 1f, 1f,
        -1f,  1f, 0f, 1f,
    )

    override fun onSurfaceCreated(gl: GL10?, config: EGLConfig?) {
        GLES31.glClearColor(0f, 0f, 0f, 1f)

        // Create compute shader program
        computeProgram = createComputeProgram()

        // Create render program (vertex + fragment shader for fullscreen quad)
        renderProgram = createRenderProgram()

        // Create output texture for compute shader
        val texIds = IntArray(1)
        GLES31.glGenTextures(1, texIds, 0)
        outputTexture = texIds[0]
        GLES31.glBindTexture(GLES31.GL_TEXTURE_2D, outputTexture)
        GLES31.glTexStorage2D(GLES31.GL_TEXTURE_2D, 1, GLES31.GL_RGBA8, TEX_WIDTH, TEX_HEIGHT)
        GLES31.glTexParameteri(GLES31.GL_TEXTURE_2D, GLES31.GL_TEXTURE_MIN_FILTER, GLES31.GL_LINEAR)
        GLES31.glTexParameteri(GLES31.GL_TEXTURE_2D, GLES31.GL_TEXTURE_MAG_FILTER, GLES31.GL_LINEAR)

        // Create fullscreen quad VAO/VBO
        val vaoIds = IntArray(1)
        GLES31.glGenVertexArrays(1, vaoIds, 0)
        quadVAO = vaoIds[0]

        val vboIds = IntArray(1)
        GLES31.glGenBuffers(1, vboIds, 0)
        quadVBO = vboIds[0]

        GLES31.glBindVertexArray(quadVAO)
        GLES31.glBindBuffer(GLES31.GL_ARRAY_BUFFER, quadVBO)

        val buffer: FloatBuffer = ByteBuffer.allocateDirect(quadVertices.size * 4)
            .order(ByteOrder.nativeOrder())
            .asFloatBuffer()
            .put(quadVertices)
        buffer.position(0)

        GLES31.glBufferData(GLES31.GL_ARRAY_BUFFER, quadVertices.size * 4, buffer, GLES31.GL_STATIC_DRAW)

        // Position attribute
        GLES31.glVertexAttribPointer(0, 2, GLES31.GL_FLOAT, false, 16, 0)
        GLES31.glEnableVertexAttribArray(0)

        // TexCoord attribute
        GLES31.glVertexAttribPointer(1, 2, GLES31.GL_FLOAT, false, 16, 8)
        GLES31.glEnableVertexAttribArray(1)

        GLES31.glBindVertexArray(0)

        // Get uniform locations
        GLES31.glUseProgram(computeProgram)
        timeUniform = GLES31.glGetUniformLocation(computeProgram, "u_time")
        resolutionUniform = GLES31.glGetUniformLocation(computeProgram, "u_resolution")

        Log.i(TAG, "Surface created: compute=$computeProgram render=$renderProgram tex=$outputTexture")
    }

    override fun onSurfaceChanged(gl: GL10?, w: Int, h: Int) {
        width = w
        height = h
        GLES31.glViewport(0, 0, w, h)
    }

    override fun onDrawFrame(gl: GL10?) {
        val time = System.nanoTime() / 1_000_000_000f

        if (phantomCoreEnabled) {
            // In Phantom Core mode: the compute dispatch would be intercepted
            // by eBPF and offloaded to the PC. For the demo, we still run locally
            // but with the full compute shader (in production, this ioctl would
            // be caught and the framebuffer would arrive from the PC).
            dispatchComputeShader(time)
        } else {
            // Local mode: run the compute shader directly (heavy — will be slow)
            dispatchComputeShader(time)
        }

        // Render the output texture to screen
        GLES31.glClear(GLES31.GL_COLOR_BUFFER_BIT)
        GLES31.glUseProgram(renderProgram)
        GLES31.glActiveTexture(GLES31.GL_TEXTURE0)
        GLES31.glBindTexture(GLES31.GL_TEXTURE_2D, outputTexture)
        GLES31.glBindVertexArray(quadVAO)
        GLES31.glDrawArrays(GLES31.GL_TRIANGLES, 0, 6)
        GLES31.glBindVertexArray(0)

        // FPS tracking
        frameCount++
        val now = System.nanoTime()
        val elapsed = (now - lastFpsTime) / 1_000_000_000.0
        if (elapsed >= 1.0) {
            currentFps = (frameCount / elapsed).toInt()
            frameCount = 0
            lastFpsTime = now
        }
    }

    private fun dispatchComputeShader(time: Float) {
        GLES31.glUseProgram(computeProgram)
        GLES31.glUniform1f(timeUniform, time)
        GLES31.glUniform2f(resolutionUniform, TEX_WIDTH.toFloat(), TEX_HEIGHT.toFloat())
        GLES31.glBindImageTexture(0, outputTexture, 0, false, 0, GLES31.GL_WRITE_ONLY, GLES31.GL_RGBA8)

        // Dispatch compute: one invocation per pixel, workgroup size 16x16
        GLES31.glDispatchCompute(TEX_WIDTH / 16, TEX_HEIGHT / 16, 1)
        GLES31.glMemoryBarrier(GLES31.GL_SHADER_IMAGE_ACCESS_BARRIER_BIT)
    }

    private fun createComputeProgram(): Int {
        val source = loadShaderAsset("shaders/raytrace.comp")
        val shader = GLES31.glCreateShader(GLES31.GL_COMPUTE_SHADER)
        GLES31.glShaderSource(shader, source)
        GLES31.glCompileShader(shader)

        val status = IntArray(1)
        GLES31.glGetShaderiv(shader, GLES31.GL_COMPILE_STATUS, status, 0)
        if (status[0] == 0) {
            val log = GLES31.glGetShaderInfoLog(shader)
            Log.e(TAG, "Compute shader compile failed: $log")
        }

        val program = GLES31.glCreateProgram()
        GLES31.glAttachShader(program, shader)
        GLES31.glLinkProgram(program)

        GLES31.glGetProgramiv(program, GLES31.GL_LINK_STATUS, status, 0)
        if (status[0] == 0) {
            val log = GLES31.glGetProgramInfoLog(program)
            Log.e(TAG, "Compute program link failed: $log")
        }

        GLES31.glDeleteShader(shader)
        return program
    }

    private fun createRenderProgram(): Int {
        val vertSrc = loadShaderAsset("shaders/quad.vert")
        val fragSrc = loadShaderAsset("shaders/quad.frag")

        val vertShader = GLES31.glCreateShader(GLES31.GL_VERTEX_SHADER)
        GLES31.glShaderSource(vertShader, vertSrc)
        GLES31.glCompileShader(vertShader)

        val fragShader = GLES31.glCreateShader(GLES31.GL_FRAGMENT_SHADER)
        GLES31.glShaderSource(fragShader, fragSrc)
        GLES31.glCompileShader(fragShader)

        val program = GLES31.glCreateProgram()
        GLES31.glAttachShader(program, vertShader)
        GLES31.glAttachShader(program, fragShader)
        GLES31.glLinkProgram(program)

        val status = IntArray(1)
        GLES31.glGetProgramiv(program, GLES31.GL_LINK_STATUS, status, 0)
        if (status[0] == 0) {
            Log.e(TAG, "Render program link failed: ${GLES31.glGetProgramInfoLog(program)}")
        }

        GLES31.glDeleteShader(vertShader)
        GLES31.glDeleteShader(fragShader)
        return program
    }

    private fun loadShaderAsset(path: String): String {
        return context.assets.open(path).bufferedReader().readText()
    }
}
