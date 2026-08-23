#include "ffeshader.h"
#include "support/log.h"

#include <sstream>

using std::string;
using std::stringstream;
static string buildArgString(DWORD arg, const string& mask, const string& sampler) {
    stringstream s;

    switch (arg) {
    case D3DTA_DIFFUSE:
        s << "diffuse" << mask;
        break;
    case D3DTA_CURRENT:
        s << "c" << mask;
        break;
    case D3DTA_TEXTURE:
        s << sampler << mask;
        break;
    default:
        s << "unsupported";
        break;
    }

    return s.str();
}

bool FixedFunctionShader::buildShaderSource(const ShaderKey& sk, ShaderSource& src) {
    stringstream buf;

    // Identify output texcoords and check for texgen; supports max. one per shader
    int texGen = 0, texGenSrcIndex = 0, texGenOutputIndex = sk.uvSets, totalOutputCoords = sk.uvSets;
    if (sk.usesTexgen) {
        texGen = sk.stage[sk.texgenStage].texcoordGen;
        texGenSrcIndex = sk.stage[sk.texgenStage].texcoordIndex;

        ++totalOutputCoords;
        if (sk.projectiveTexgen) {
            ++totalOutputCoords;
        }
    }

    if (totalOutputCoords > 4) {
        LOG::logline("!! Shader generator error: excessive texcoord usage (%d).", totalOutputCoords);
        sk.log();
        LOG::flush();
        return false;
    }

    // Pack 2d texcoords into interpolators and map to stages
    const char* strInterpolators[] = { "01", "23" };
    const char* strTexcoordPacking[] = { ".xy", ".zw" };
    string texcoordNames[8], texSamplers[8];

    for (int i = 0; i != sk.activeStages; ++i) {
        bool isTexGen = bool(sk.stage[i].texcoordGen);
        int x = isTexGen ? texGenOutputIndex : sk.stage[i].texcoordIndex;

        buf.str(string());
        buf << "IN.texcoord" << strInterpolators[x >> 1] << strTexcoordPacking[x & 1];
        if (isTexGen && sk.projectiveTexgen) {
            buf << " / IN.texcoord" << strInterpolators[(x+1) >> 1] << strTexcoordPacking[(x+1) & 1];
        }
        texcoordNames[i] = buf.str();
        buf.str(string());
        buf << "tex2D(sampFFE" << i << ", " << texcoordNames[i] << ")";
        texSamplers[i] = buf.str();
    }

    // Vertex format coupling, generate equivalent struct to input FVF
    buf.str(string());

    if (sk.usesSkinning) {
        buf << "float4 blendweights : BLENDWEIGHT; ";
        if (sk.indexedSkinning) {
            buf << "float4 blendindices : BLENDINDICES; ";
        }
    }
    if (sk.vertexColour) {
        buf << "float4 col : COLOR; ";
    }
    for (int i = 0; i != sk.uvSets; ++i) {
        buf << "float2 texcoord" << i << " : TEXCOORD" << i << "; ";
    }

    src.genVBCoupling = buf.str();

    // Pixel shader coupling, passes texcoords and colours
    buf.str(string());

    if (sk.vertexColour) {
        buf << "centroid float4 col : COLOR; ";
    }
    if (totalOutputCoords == 1) {
        buf << "float2 texcoord01 : TEXCOORD0; ";
    } else if (totalOutputCoords > 1) {
        buf << "float4 texcoord01 : TEXCOORD0; ";
    }
    if (totalOutputCoords == 3) {
        buf << "float2 texcoord23 : TEXCOORD1; ";
    } else if (totalOutputCoords == 4) {
        buf << "float4 texcoord23 : TEXCOORD1; ";
    }

    src.genPSCoupling = buf.str();

    // Transform / skinning
    buf.str(string());

    if (sk.indexedSkinning) {
        buf << "viewpos = indexedSkinnedVertex(IN.pos, IN.blendweights, IN.blendindices); normal = indexedSkinnedNormal(IN.nrm, IN.blendweights, IN.blendindices);";
    } else if (sk.usesSkinning) {
        buf << "viewpos = skinnedVertex(IN.pos, IN.blendweights); normal = skinnedNormal(IN.nrm, IN.blendweights);";
    } else {
        buf << "viewpos = rigidVertex(IN.pos); normal = rigidNormal(IN.nrm);";
    }

    src.genTransform = buf.str();

    // Texcoord routing and texgen
    string texRouting[4];
    for (int i = 0; i != sk.uvSets; ++i) {
        buf.str(string());
        buf << "IN.texcoord" << i;
        texRouting[i] = buf.str();
    }

    buf.str(string());

    if (texGen) {
        buf << "float3 texgen = ";
        switch (texGen) {
        case D3DTSS_TCI_CAMERASPACENORMAL >> 16:
            buf << "texgenNormal(normal); ";
            break;
        case D3DTSS_TCI_CAMERASPACEPOSITION >> 16:
            buf << "texgenPosition(viewpos); ";
            break;
        case D3DTSS_TCI_CAMERASPACEREFLECTIONVECTOR >> 16:
            buf << "texgenReflection(viewpos, normal); ";
            break;
        case D3DTSS_TCI_SPHEREMAP >> 16:
            buf << "texgenSphere(" << texRouting[texGenSrcIndex] << "); ";
            break;
        }
        buf << "texgen = mul(float4(texgen, 1), texgenTransform).xyz; ";
        texRouting[texGenOutputIndex] = "texgen.xy";
        if (sk.projectiveTexgen) {
            texRouting[texGenOutputIndex + 1] = "texgen.zz";
        }
    }

    if (totalOutputCoords == 1) {
        buf << "OUT.texcoord01 = " << texRouting[0] << ";";
    } else if (totalOutputCoords > 1) {
        buf << "OUT.texcoord01 = float4(" << texRouting[0] << ", " << texRouting[1] << "); ";
    }
    if (totalOutputCoords == 3) {
        buf << "OUT.texcoord23 = " << texRouting[2] << ";";
    } else if (totalOutputCoords == 4) {
        buf << "OUT.texcoord23 = float4(" << texRouting[2] << ", " << texRouting[3] << ");";
    }

    src.genTexcoords = buf.str();

    // Vertex colour routing
    buf.str(string());
    if (sk.vertexColour) {
        buf << "OUT.col = IN.col;";
    }
    src.genVertexColour = buf.str();

    // Lighting
    if (sk.vertexMaterial == 0) {
        src.genLightCount = "0";
    } else {
        src.genLightCount = sk.heavyLighting ? "8" : "4";
    }

    // Vertex material
    buf.str(string());
    switch (sk.vertexMaterial) {
    case 0:
        buf << "diffuse = " << (sk.vertexColour ? "IN.col;" : "1.0;");
        break;
    case 1:
        buf << "diffuse = vertexMaterialNone(d, a);";
        break;
    case 2:
        buf << "diffuse = vertexMaterialDiffAmb(d, a, IN.col);";
        break;
    case 3:
        buf << "diffuse = vertexMaterialEmissive(d, a, IN.col);";
        break;
    }
    src.genMaterial = buf.str();

    // Texture and shading operations
    buf.str(string());
    string arg1, arg2, arg3;

    for (int i = 0; i != sk.activeStages; ++i) {
        const ShaderKey::Stage& s = sk.stage[i];
        const string dest = s.alphaOpMatched ? "c = " : "c.rgb = ";
        const string mask = s.alphaOpMatched ? "" : ".rgb";

        arg1 = buildArgString(s.colorArg1, mask, texSamplers[i]);
        arg2 = buildArgString(s.colorArg2, mask, texSamplers[i]);

        switch (s.colorOp) {
        case D3DTOP_SELECTARG1:
            buf << dest << arg1 << ";";
            break;

        case D3DTOP_SELECTARG2:
            buf << dest << arg2 << ";";
            break;

        case D3DTOP_MODULATE:
            buf << dest << arg1 << " * " << arg2 << ";";
            break;

        case D3DTOP_MODULATE2X:
            buf << dest << "2 * " << arg1 << " * " << arg2 << ";";
            break;

        case D3DTOP_MODULATE4X:
            buf << dest << "4 * " << arg1 << " * " << arg2 << ";";
            break;

        case D3DTOP_ADD:
            buf << dest << arg1 << " + " << arg2 << ";";
            break;

        case D3DTOP_ADDSIGNED:
            buf << dest << arg1 << " + " << arg2 << " - 0.5;";
            break;

        case D3DTOP_ADDSIGNED2X:
            buf << dest << "2 * (" << arg1 << "+" << arg2 << ") - 1;";
            break;

        case D3DTOP_SUBTRACT:
            buf << dest << arg1 << " - " << arg2 << ";";
            break;

        case D3DTOP_BLENDDIFFUSEALPHA:
            buf << dest << "lerp(" << arg1 << ", " << arg2 << ", diffuse.a);";
            break;

        case D3DTOP_BLENDTEXTUREALPHA:
            arg3 = buildArgString(D3DTA_TEXTURE, "", texSamplers[i]);
            buf << "float4 temp" << i << " = " << arg3 << "; lerp(" << arg1 << ", " << arg1 << ", temp" << i << ".a);";
            break;

        case D3DTOP_BUMPENVMAP:
            arg3 = buildArgString(D3DTA_TEXTURE, "", texSamplers[i]);
            buf << "float4 bump = bumpmapStage(sampFFE" << i+1 << ", " << texcoordNames[i+1] << ", " << arg3 << ");";
            texSamplers[i+1] = "bump";
            break;

        case D3DTOP_BUMPENVMAPLUMINANCE:
            arg3 = buildArgString(D3DTA_TEXTURE, "", texSamplers[i]);
            buf << "float4 bump = bumpmapLumiStage(sampFFE" << i+1 << ", " << texcoordNames[i+1] << ", " << arg3 << ");";
            texSamplers[i+1] = "bump";
            break;

        case D3DTOP_DOTPRODUCT3:
            arg1 = buildArgString(s.colorArg1, ".rgb", texSamplers[i]);
            arg2 = buildArgString(s.colorArg2, ".rgb", texSamplers[i]);
            buf << "c.rgb = dot(" << arg1 << ", " << arg2 << ");";
            break;

        case D3DTOP_MULTIPLYADD:
            arg1 = buildArgString(s.colorArg1, ".rgb", texSamplers[i]);
            arg2 = buildArgString(s.colorArg2, ".rgb", texSamplers[i]);
            arg3 = buildArgString(s.colorArg0, ".rgb", texSamplers[i]);
            buf << "c.rgb = " << arg1 << " * " << arg2 << " + " << arg3 << ";";
            break;

        default:
            buf << "unsupported";
            break;
        }

        if (s.alphaOpSelect1) {
            // Alpha Select1 op, assumes alpha args are the same as color args
            switch (s.colorArg1) {
            case D3DTA_DIFFUSE:
                buf << "c.a = diffuse.a";
                break;

            case D3DTA_TEXTURE:
                // The HLSL compiler is able to optimize this repeated sampler use and does not generate an extra texld.
                buf << "c.a = " << texSamplers[i] << ".a;";
                break;
            }
        }

        buf << " \\\n";
    }

    src.genTexturing = buf.str();

    // Final fog application
    buf.str(string());

    switch (sk.fogMode) {
    case 0:     // Fog disabled
        break;
    case 1:     // Standard fog mode
        buf << "c.rgb = lerp(fogColNear, c.rgb, fog); ";
        break;
    case 2:     // Additive objects should fog towards black, which preserves the destination correctly
        buf << "c.rgb *= fog; ";
        break;
    }

    src.genFog = buf.str();

    src.macros.push_back({ "FFE_VB_COUPLING", src.genVBCoupling.c_str() });
    src.macros.push_back({ "FFE_SHADER_COUPLING", src.genPSCoupling.c_str() });
    src.macros.push_back({ "FFE_TRANSFORM_SKIN", src.genTransform.c_str() });
    src.macros.push_back({ "FFE_TEXCOORDS_TEXGEN", src.genTexcoords.c_str() });
    src.macros.push_back({ "FFE_VERTEX_COLOUR", src.genVertexColour.c_str() });
    src.macros.push_back({ "FFE_LIGHTS_ACTIVE", src.genLightCount.c_str() });
    src.macros.push_back({ "FFE_VERTEX_MATERIAL", src.genMaterial.c_str() });
    src.macros.push_back({ "FFE_TEXTURING", src.genTexturing.c_str() });
    src.macros.push_back({ "FFE_FOG_APPLICATION", src.genFog.c_str() });
    src.macros.push_back({ 0, 0 });

    return true;
}
