#version 310 es
// PhantomCore — Fullscreen Quad Fragment Shader
// Samples from the compute shader output texture

precision highp float;

in highp vec2 vTexCoord;
out highp vec4 fragColor;

uniform sampler2D uTexture;

void main() {
    fragColor = texture(uTexture, vTexCoord);
}
