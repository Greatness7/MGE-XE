
// XE Shadowmap.fx
// MGE XE 0.16.0
// Shadow map rendering

#include "XE Common.fx"
#include "XE Mod Shadow Data.fx"



//------------------------------------------------------------
// Shadow caster rendering

struct ShadowVertOut {
    float4 pos : POSITION;
    float depth : TEXCOORD0;
};

// Position only. Callers supply the stencil clip cube (WaterDecl) and distant terrain
// (TerrainDecl), neither of which carries texcoords, so terrain casters cannot alpha test.
ShadowVertOut ShadowVS(float4 pos : POSITION) {
    ShadowVertOut OUT;

    OUT.pos = mul(pos, world);
    OUT.pos = mul(OUT.pos, shadowViewProj[0]);

    // Clamp vertices to front plane to avoid clipping and shadow loss
    OUT.pos.z = max(0, OUT.pos.z);

    // Output depth (ortho projection is linear)
    OUT.depth = OUT.pos.z / OUT.pos.w;
    return OUT;
}

ShadowVertOut ShadowClearVS(float4 pos : POSITION) {
    ShadowVertOut OUT;

    OUT.pos = pos;
    OUT.depth = 1.0f;
    return OUT;
}

float4 ShadowPS(ShadowVertOut IN) : COLOR0 {
    return ESM_scale * IN.depth;
}

struct StaticShadowVertOut {
    float4 pos : POSITION;
    float2 texcoords : TEXCOORD0;
    float depth : TEXCOORD1;
    float4 uvBounds : TEXCOORD2;
};

StaticShadowVertOut StaticShadowVS(StatVertIn IN) {
    StaticShadowVertOut OUT;

    // pos.w carries the palette ordinal, so it must not scale the world translation.
    OUT.pos = mul(float4(IN.pos.xyz, 1), world);
    OUT.pos = mul(OUT.pos, shadowViewProj[0]);

    // Clamp vertices to front plane to avoid clipping and shadow loss
    OUT.pos.z = max(0, OUT.pos.z);

    // Output depth (ortho projection is linear)
    OUT.depth = OUT.pos.z / OUT.pos.w;

    OUT.texcoords = IN.texcoords;
    OUT.uvBounds = uvBoundPalette[(int)IN.pos.w];
    return OUT;
}

float4 StaticShadowPS(StaticShadowVertOut IN) : COLOR0 {
    // Sample the static's assigned atlas region if alpha testing is required
    if(hasAlpha) {
        float2 scale = IN.uvBounds.yw - IN.uvBounds.zx;
        float2 dx = ddx(IN.texcoords) * scale;
        float2 dy = ddy(IN.texcoords) * scale;
        float2 atlasUV = IN.uvBounds.zx + frac(IN.texcoords) * scale;
        float a = tex2Dgrad(sampBaseTex, atlasUV, dx, dy).a;
        clip(a - 180.0/255.0);
    }

    return ESM_scale * IN.depth;
}

float4 ShadowStencilPS(ShadowVertOut IN) : COLOR0 {
    return 1;
}

//------------------------------------------------------------
// Shadow map filtering

static const float2 shadowAtlasRcpRes = shadowRcpRes * float2(shadowCascadeSize, 1);

struct ShadowPostOut {
    float4 pos : POSITION;
    float2 texcoords : TEXCOORD0;
};

ShadowPostOut ShadowSoftenVS(float4 pos : POSITION) {
    ShadowPostOut OUT;

    OUT.pos = pos;
    OUT.texcoords = (0.5 + 0.5*shadowAtlasRcpRes) + float2(0.5, -0.5) * pos.xy;
    return OUT;
}

// Filter entire atlas along one axis. The two passes below run in sequence to make a
// separable blur. Looks better without exp-space filtering, with a side effect of
// expanding silhouettes by about 1 pixel.
float4 shadowSoften(float2 texcoords, float2 axis) {
    float4 t = float4(texcoords, 0, 0);
    float4 offset = float4(shadowRcpRes.x * axis, 0, 0);
    float d = tex2Dlod(sampDepth, t).r;

    d += 0.2 * tex2Dlod(sampDepth, t - 1.42*offset).r;
    d += 0.8 * tex2Dlod(sampDepth, t - 0.71*offset).r;
    d += 0.8 * tex2Dlod(sampDepth, t + 0.71*offset).r;
    d += 0.2 * tex2Dlod(sampDepth, t + 1.42*offset).r;

    return (d / 3.0).xxxx;
}

float4 ShadowSoftenHorizontalPS(ShadowPostOut IN) : COLOR0 {
    return shadowSoften(IN.texcoords, float2(1, 0));
}

float4 ShadowSoftenVerticalPS(ShadowPostOut IN) : COLOR0 {
    return shadowSoften(IN.texcoords, float2(0, 1));
}

//-----------------------------------------------------------------------------

technique T0 {
    //------------------------------------------------------------
    // Used to clear the shadow map
    Pass P0 {
        ZEnable = false;
        ZWriteEnable = false;
        ZFunc = LessEqual;
        CullMode = CW;

        AlphaBlendEnable = false;
        AlphaTestEnable = false;
        FogEnable = false;
        Lighting = false;

        VertexShader = compile vs_3_0 ShadowClearVS();
        PixelShader = compile ps_3_0 ShadowPS();
    }
    //------------------------------------------------------------
    // Used to render the view frustum into the stencil
    Pass P1 {
        ZEnable = false;
        ZWriteEnable = false;
        ColorWriteEnable = 0;
        CullMode = none;

        StencilEnable = true;
        StencilFunc = always;
        StencilPass = replace;
        StencilFail = keep;
        StencilRef = 1;
        StencilMask = 0xffffffff;

        VertexShader = compile vs_3_0 ShadowVS();
        PixelShader = compile ps_3_0 ShadowStencilPS();
    }
    //------------------------------------------------------------
    // Used to render the shadow map
    Pass P2 {
        ZEnable = true;
        ZWriteEnable = true;
        ColorWriteEnable = red|green|blue|alpha;
        CullMode = CW;

        StencilEnable = true;
        StencilFunc = notequal;
        StencilPass = keep;
        StencilFail = keep;
        StencilRef = 0;
        StencilMask = 0xffffffff;

        VertexShader = compile vs_3_0 ShadowVS();
        PixelShader = compile ps_3_0 ShadowPS();
    }
    //------------------------------------------------------------
    // Used to render distant statics into the shadow map
    Pass P3 {
        ZEnable = true;
        ZWriteEnable = true;
        ColorWriteEnable = red|green|blue|alpha;
        CullMode = CW;

        StencilEnable = true;
        StencilFunc = notequal;
        StencilPass = keep;
        StencilFail = keep;
        StencilRef = 0;
        StencilMask = 0xffffffff;

        VertexShader = compile vs_3_0 StaticShadowVS();
        PixelShader = compile ps_3_0 StaticShadowPS();
    }
    //------------------------------------------------------------
    // Used to soften the shadow map, horizontal half of the separable blur
    Pass P4 {
        ZEnable = false;
        ZWriteEnable = false;

        StencilEnable = false;

        VertexShader = compile vs_3_0 ShadowSoftenVS();
        PixelShader = compile ps_3_0 ShadowSoftenHorizontalPS();
    }
    //------------------------------------------------------------
    // Vertical half of the separable blur
    Pass P5 {
        ZEnable = false;
        ZWriteEnable = false;

        StencilEnable = false;

        VertexShader = compile vs_3_0 ShadowSoftenVS();
        PixelShader = compile ps_3_0 ShadowSoftenVerticalPS();
    }
    //------------------------------------------------------------
}
