#version 310 es
// PhantomCore — Fullscreen Quad Vertex Shader

layout(location = 0) in vec2 aPosition;
layout(location = 1) in vec2 aTexCoord;

out highp vec2 vTexCoord;

void main() {
    gl_Position = vec4(aPosition, 0.0, 1.0);
    vTexCoord = aTexCoord;
}
