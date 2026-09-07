#pragma once

// NetImmerse / Gamebryo runtime types as laid out by Morrowind.exe v1.6.1820.
//
// Naming authority is MWSE's `SharedSE/NI*.h`; see docs/architecture/mwbridge.md.
// Only the members MGE actually touches are named. Everything else is explicit
// padding, sized so the `static_assert(sizeof(...))` below stays honest -- those
// asserts are what makes this header a checkable transcription rather than a
// pile of hopeful offsets.

#include <cstddef>
#include <cstdint>

struct IDirect3DDevice8;
struct IDirect3DTexture8;

namespace NI {

//-----------------------------------------------------------------------------
// Support types
//-----------------------------------------------------------------------------

struct Object;

// MWSE's Object_vTable carries 11 entries; only the destructor is called from
// here, so the rest stay opaque slots. The size assertion is what keeps the
// slot count honest.
struct ObjectVTable {
    void(__thiscall* destructor)(Object*, int);  // 0x00
    void* entries[10];                           // 0x04
};
static_assert(sizeof(ObjectVTable) == 0x2C, "NI::ObjectVTable failed size validation");

struct Object {
    ObjectVTable* vTable;  // 0x00
    int refCount;          // 0x04
};
static_assert(sizeof(Object) == 0x8, "NI::Object failed size validation");

// NiPointer<T>, the engine's intrusive refcounted handle.
//
// The ownership is load-bearing, not decoration: assigning through the handle
// is what releases an object back to the engine (see the partition rebuild in
// morrowindskinning.cpp), and MGE holds strong references of its own in the
// stock-geometry map. Reading an engine-owned field through operator-> or
// operator T* touches no refcount, which is all the overlay structs below do.
template <class T>
class Pointer {
public:
    Pointer(T* pointer = nullptr) {
        claim(pointer);
    }

    Pointer(const Pointer<T>& other) {
        claim(other.m_pointer);
    }

    ~Pointer() {
        release();
    }

    Pointer<T>& operator=(const Pointer<T>& other) {
        if (m_pointer != other.m_pointer) {
            claim(other.m_pointer);
        }
        return *this;
    }

    Pointer<T>& operator=(T* pointer) {
        if (m_pointer != pointer) {
            claim(pointer);
        }
        return *this;
    }

    operator T* () const {
        return m_pointer;
    }

    T* operator->() const {
        return m_pointer;
    }

    T* get() const {
        return m_pointer;
    }

private:
    T* m_pointer = nullptr;

    void release() {
        if (m_pointer) {
            T* const released = m_pointer;
            m_pointer = nullptr;
            if (--released->refCount == 0) {
                released->vTable->destructor(static_cast<Object*>(released), 1);
            }
        }
    }

    void claim(T* pointer) {
        release();
        m_pointer = pointer;
        if (m_pointer) {
            m_pointer->refCount++;
        }
    }
};
static_assert(sizeof(Pointer<Object>) == 0x4, "NI::Pointer failed size validation");

template <typename T>
struct LinkedList {
    T* data;            // 0x00
    LinkedList<T>* next;  // 0x04
};
static_assert(sizeof(LinkedList<int>) == 0x8, "NI::LinkedList failed size validation");

struct Point2 {
    float x, y;
};
static_assert(sizeof(Point2) == 0x8, "NI::Point2 failed size validation");

struct Point3 {
    float x, y, z;
};
static_assert(sizeof(Point3) == 0xC, "NI::Point3 failed size validation");

struct Point4 {
    float x, y, z, w;
};
static_assert(sizeof(Point4) == 0x10, "NI::Point4 failed size validation");

struct Matrix33 {
    Point3 m0, m1, m2;
};
static_assert(sizeof(Matrix33) == 0x24, "NI::Matrix33 failed size validation");

struct Matrix44 {
    Point4 m0, m1, m2, m3;
};
static_assert(sizeof(Matrix44) == 0x40, "NI::Matrix44 failed size validation");

struct Transform {
    Matrix33 rotation;    // 0x00
    Point3 translation;   // 0x24
    float scale;          // 0x30
};
static_assert(sizeof(Transform) == 0x34, "NI::Transform failed size validation");

struct Bound {
    Point3 center;   // 0x00
    float radius;    // 0x0C
};
static_assert(sizeof(Bound) == 0x10, "NI::Bound failed size validation");

struct BoundingBox {
    Point3 minimum;  // 0x00
    Point3 maximum;  // 0x0C
};
static_assert(sizeof(BoundingBox) == 0x18, "NI::BoundingBox failed size validation");

// D3DCOLOR byte order on little-endian. MGE historically declared this RGBA in
// morrowindskinning.cpp; it never named a channel, so the orderings were
// indistinguishable there. BGRA is the correct one.
struct PackedColor {
    unsigned char b;  // 0x00
    unsigned char g;  // 0x01
    unsigned char r;  // 0x02
    unsigned char a;  // 0x03
};
static_assert(sizeof(PackedColor) == 0x4, "NI::PackedColor failed size validation");

struct Color {
    float r, g, b;
};
static_assert(sizeof(Color) == 0xC, "NI::Color failed size validation");

// NiTArray<T>. Virtual, so the vtable pointer is the first word.
template <typename T>
struct TArray {
    void* vTable;            // 0x00
    T* storage;              // 0x04
    size_t storageCount;     // 0x08
    size_t endIndex;         // 0x0C
    size_t filledCount;      // 0x10
    size_t growByCount;      // 0x14
};
static_assert(sizeof(TArray<void*>) == 0x18, "NI::TArray failed size validation");

//-----------------------------------------------------------------------------
// Object hierarchy
//-----------------------------------------------------------------------------

struct Property;
using PropertyLinkedList = LinkedList<Property>;

struct Node;
struct SkinInstance;

struct ObjectNET : Object {
    char* name;                 // 0x08
    Pointer<Object> extraData;  // 0x0C
    Pointer<Object> controllers;  // 0x10
};
static_assert(sizeof(ObjectNET) == 0x14, "NI::ObjectNET failed size validation");

struct AVObject : ObjectNET {
    // Bit 0 is the app-cull flag: set means "do not render".
    unsigned short flags;              // 0x14
    short pad_16;                      // 0x16
    Node* parentNode;                  // 0x18
    Point3 worldBoundOrigin;           // 0x1C
    float worldBoundRadius;            // 0x28
    Matrix33* localRotation;           // 0x2C
    Point3 localTranslate;             // 0x30
    float localScale;                  // 0x3C
    Transform worldTransform;          // 0x40
    void* velocities;                  // 0x74
    void* modelABV;                    // 0x78
    void* worldABV;                    // 0x7C
    int(__cdecl* collideCallback)(void*);  // 0x80
    void* collideCallbackUserData;     // 0x84
    PropertyLinkedList propertyNode;   // 0x88

    static constexpr unsigned short FlagAppCulled = 0x1;
};
static_assert(sizeof(AVObject) == 0x90, "NI::AVObject failed size validation");
static_assert(offsetof(AVObject, flags) == 0x14, "NI::AVObject::flags failed offset validation");
static_assert(offsetof(AVObject, worldBoundOrigin) == 0x1C, "NI::AVObject::worldBoundOrigin failed offset validation");
static_assert(offsetof(AVObject, localTranslate) == 0x30, "NI::AVObject::localTranslate failed offset validation");
static_assert(offsetof(AVObject, worldTransform) == 0x40, "NI::AVObject::worldTransform failed offset validation");
static_assert(offsetof(AVObject, propertyNode) == 0x88, "NI::AVObject::propertyNode failed offset validation");

struct Node : AVObject {
    TArray<Pointer<AVObject>> children;  // 0x90
    LinkedList<Object> effectList;       // 0xA8
};
static_assert(sizeof(Node) == 0xB0, "NI::Node failed size validation");

struct Property : ObjectNET {
    unsigned short flags;  // 0x14
    unsigned short pad_16;  // 0x16
};
static_assert(sizeof(Property) == 0x18, "NI::Property failed size validation");

struct MaterialProperty : Property {
    int index;          // 0x18
    Color ambient;      // 0x1C
    Color diffuse;      // 0x28
    Color specular;     // 0x34
    Color emissive;     // 0x40
    // Normally unused by Morrowind, which is why MGE writes recognizable values
    // here to tag the water and moon materials for the render passes.
    float shininess;    // 0x4C
    float alpha;        // 0x50
    unsigned int revisionID;  // 0x54
};
static_assert(sizeof(MaterialProperty) == 0x58, "NI::MaterialProperty failed size validation");
static_assert(offsetof(MaterialProperty, shininess) == 0x4C, "NI::MaterialProperty::shininess failed offset validation");

struct FogProperty : Property {
    float density;              // 0x18
    unsigned char color[4];     // 0x1C
};
static_assert(sizeof(FogProperty) == 0x20, "NI::FogProperty failed size validation");
static_assert(offsetof(FogProperty, density) == 0x18, "NI::FogProperty::density failed offset validation");
static_assert(offsetof(FogProperty, color) == 0x1C, "NI::FogProperty::color failed offset validation");

struct GeometryData : Object {
    unsigned short vertexCount;   // 0x08
    unsigned short textureSets;   // 0x0A
    Bound bounds;                 // 0x0C
    Point3* vertex;               // 0x1C
    Point3* normal;               // 0x20
    PackedColor* color;           // 0x24
    Point2* textureCoords;        // 0x28
    unsigned int uniqueID;        // 0x2C
    unsigned short revisionID;    // 0x30
    bool unknown_0x32;            // 0x32
    char pad_33;                  // 0x33
};
static_assert(sizeof(GeometryData) == 0x34, "NI::GeometryData failed size validation");
static_assert(offsetof(GeometryData, color) == 0x24, "NI::GeometryData::color failed offset validation");

struct Geometry : AVObject {
    void* propertyState;                 // 0x90
    void* effectState;                   // 0x94
    Pointer<GeometryData> modelData;     // 0x98
    Pointer<SkinInstance> skinInstance;  // 0x9C
    Point3* worldVertices;               // 0xA0
    Point3* worldNormals;                // 0xA4
    bool bWorldVerticesDirty;            // 0xA8
    bool bWorldNormalsDirty;             // 0xA9
    char pad_AA[2];                      // 0xAA
};
static_assert(sizeof(Geometry) == 0xAC, "NI::Geometry failed size validation");
static_assert(offsetof(Geometry, modelData) == 0x98, "NI::Geometry::modelData failed offset validation");

// Adds no data members of its own.
struct TriBasedGeometry : Geometry {};
static_assert(sizeof(TriBasedGeometry) == 0xAC, "NI::TriBasedGeometry failed size validation");

struct TriShape : TriBasedGeometry {};
static_assert(sizeof(TriShape) == 0xAC, "NI::TriShape failed size validation");

// NiTriBasedGeomData. MakePartitions reads the triangle count unconditionally,
// so every geometry that reaches it is triangle-based.
struct TriBasedGeometryData : GeometryData {
    unsigned short triangleCount;      // 0x34
    unsigned short patchRenderFlags;   // 0x36
};
static_assert(sizeof(TriBasedGeometryData) == 0x38, "NI::TriBasedGeometryData failed size validation");

//-----------------------------------------------------------------------------
// Skinning
//-----------------------------------------------------------------------------

struct Triangle {
    unsigned short vertex[3];
};
static_assert(sizeof(Triangle) == 0x6, "NI::Triangle failed size validation");

struct SkinPartition : Object {
    struct Partition {
        void* vtbl;                        // 0x00
        unsigned short* bones;             // 0x04
        float* weights;                    // 0x08
        unsigned short* vertices;          // 0x0C
        unsigned char* bonePalette;        // 0x10
        Triangle* triangles;               // 0x14
        unsigned short* stripLengths;      // 0x18
        unsigned short numVertices;        // 0x1C
        unsigned short numTriangles;       // 0x1E
        unsigned short numBones;           // 0x20
        unsigned short numStripLengths;    // 0x22
        unsigned short numBonesPerVertex;  // 0x24
        char pad_26[0x2];                  // 0x26
        void* bufferData;                  // 0x28
    };

    unsigned int partitionCount;  // 0x08
    Partition* partitions;        // 0x0C
};
static_assert(sizeof(SkinPartition) == 0x10, "NI::SkinPartition failed size validation");
static_assert(sizeof(SkinPartition::Partition) == 0x2C, "NI::SkinPartition::Partition failed size validation");

struct SkinData : Object {
    struct BoneData {
        struct VertexWeight {
            unsigned short index;   // 0x00
            char pad_02[0x2];       // 0x02
            float weight;           // 0x04
        };

        Transform transform;         // 0x00
        Bound bounds;                // 0x34
        VertexWeight* weights;       // 0x44
        unsigned short weightCount;  // 0x48
        char pad_4A[0x2];            // 0x4A
    };

    // Assigning here releases the previous partition back to the engine.
    Pointer<SkinPartition> partition;  // 0x08
    Transform transform;               // 0x0C  passed through only
    unsigned int numBones;             // 0x40
    BoneData* boneData;                // 0x44
};
static_assert(sizeof(SkinData) == 0x48, "NI::SkinData failed size validation");
static_assert(sizeof(SkinData::BoneData) == 0x4C, "NI::SkinData::BoneData failed size validation");
static_assert(sizeof(SkinData::BoneData::VertexWeight) == 0x8, "NI::SkinData::BoneData::VertexWeight failed size validation");

struct SkinInstance : Object {
    Pointer<SkinData> skinData;  // 0x08
    AVObject* rootParent;        // 0x0C
    AVObject** bones;            // 0x10
    int unknown_0x14;            // 0x14
};
static_assert(sizeof(SkinInstance) == 0x18, "NI::SkinInstance failed size validation");

//-----------------------------------------------------------------------------
// Camera
//-----------------------------------------------------------------------------

struct Frustum {
    float left;    // 0x00
    float right;   // 0x04
    float top;     // 0x08
    float bottom;  // 0x0C
    // MWSE names these `near` and `far`. Renamed here because `minwindef.h`
    // defines both as empty macros for 16-bit source compatibility, and it
    // defines `FAR` as `far` -- so undefining them to recover MWSE's spelling
    // breaks DEFINE_GUID throughout the DirectX headers.
    float nearPlane;  // 0x10
    float farPlane;   // 0x14
};
static_assert(sizeof(Frustum) == 0x18, "NI::Frustum failed size validation");
static_assert(offsetof(Frustum, farPlane) == 0x14, "NI::Frustum::farPlane failed offset validation");

struct Camera : AVObject {
    Matrix44 worldToCamera;    // 0x90
    float viewDistance;        // 0xD0
    float twoDivRmL;           // 0xD4
    float twoDivTmB;           // 0xD8
    Point3 worldDirection;     // 0xDC
    Point3 worldUp;            // 0xE8
    Point3 worldRight;         // 0xF4
    Frustum viewFrustum;       // 0x100
    Point4 port;               // 0x118
    Pointer<Node> scene;       // 0x128
    char pad_12C[0xB4];        // 0x12C
};
static_assert(sizeof(Camera) == 0x1E0, "NI::Camera failed size validation");
static_assert(offsetof(Camera, worldUp) == 0xE8, "NI::Camera::worldUp failed offset validation");
static_assert(offsetof(Camera, viewFrustum) == 0x100, "NI::Camera::viewFrustum failed offset validation");
static_assert(offsetof(Camera, scene) == 0x128, "NI::Camera::scene failed offset validation");

//-----------------------------------------------------------------------------
// Renderer
//-----------------------------------------------------------------------------

struct DX8RenderTarget {
    unsigned int width;    // 0x00
    unsigned int height;   // 0x04
    char pad_08[0x1C];     // 0x08
};
static_assert(sizeof(DX8RenderTarget) == 0x24, "NI::DX8RenderTarget failed size validation");

struct DX8VertexBufferManager {
    void* vTable;                 // 0x00
    int unknown_0x4;              // 0x04
    IDirect3DDevice8* d3dDevice;  // 0x08
};
static_assert(sizeof(DX8VertexBufferManager) == 0xC, "NI::DX8VertexBufferManager failed size validation");

// Guards NiDX8VertexBufferManager's buffer creation. Opaque; only ever passed
// back to the engine's own lock and unlock.
struct CriticalSection;

// NiDX8Renderer. MWSE splits an abstract `Renderer` base out of this; MGE only
// ever holds the concrete DX8 one, so the base is folded in.
struct DX8Renderer : Object {
    char pad_08[0x14];                       // 0x08
    void* propertyStatePtr;                  // 0x1C
    void* effectStatePtr;                     // 0x20
    IDirect3DDevice8* d3dDevice;             // 0x24
    void* deviceWindowHandle;                // 0x28
    char pad_2C[0x4F4];                      // 0x2C
    DX8RenderTarget backbufferRenderTarget;  // 0x520
    DX8RenderTarget* currentRenderTarget;    // 0x544
    char pad_548[0x158];                     // 0x548
};
static_assert(sizeof(DX8Renderer) == 0x6A0, "NI::DX8Renderer failed size validation");
static_assert(offsetof(DX8Renderer, d3dDevice) == 0x24, "NI::DX8Renderer::d3dDevice failed offset validation");
static_assert(offsetof(DX8Renderer, backbufferRenderTarget) == 0x520, "NI::DX8Renderer::backbufferRenderTarget failed offset validation");
static_assert(offsetof(DX8Renderer, currentRenderTarget) == 0x544, "NI::DX8Renderer::currentRenderTarget failed offset validation");

//-----------------------------------------------------------------------------
// Picking
//-----------------------------------------------------------------------------

// SharedSE types the two object members as Pointer<T>. They are raw here on
// purpose: MGE never owns pick results, and funcphysics.cpp copies a whole
// PickRecord by value into a static, which through an owning handle would take
// a strong reference on every raycast and never release it.
struct PickRecord {
    Geometry* object;                // 0x00
    AVObject* proxyParent;           // 0x04
    Point3 intersection;             // 0x08
    float distance;                  // 0x14
    unsigned short triangleIndex;    // 0x18
    unsigned short vertexIndex[3];   // 0x1A
    Point2 texture;                  // 0x20
    Point3 normal;                   // 0x28
    PackedColor color;               // 0x34
};
static_assert(sizeof(PickRecord) == 0x38, "NI::PickRecord failed size validation");
static_assert(offsetof(PickRecord, distance) == 0x14, "NI::PickRecord::distance failed offset validation");
static_assert(offsetof(PickRecord, normal) == 0x28, "NI::PickRecord::normal failed offset validation");

struct Pick {
    int pickType;                   // 0x00
    int sortType;                   // 0x04
    int intersectType;              // 0x08
    int coordinateType;             // 0x0C
    bool frontOnly;                 // 0x10
    bool observeAppCullFlag;        // 0x11
    bool unknown_0x12;              // 0x12
    char pad_13;                    // 0x13
    Node* root;                     // 0x14
    TArray<PickRecord*> results;    // 0x18
    PickRecord* lastAddedRecord;    // 0x30
    bool returnTexture;             // 0x34
    bool returnNormal;              // 0x35
    bool returnSmoothNormal;        // 0x36
    bool returnColor;               // 0x37
};
static_assert(sizeof(Pick) == 0x38, "NI::Pick failed size validation");
static_assert(offsetof(Pick, root) == 0x14, "NI::Pick::root failed offset validation");
static_assert(offsetof(Pick, results) == 0x18, "NI::Pick::results failed offset validation");

//-----------------------------------------------------------------------------
// Textures and files
//-----------------------------------------------------------------------------

struct PixelFormat {
    int format;                     // 0x00
    unsigned int channelMasks[4];   // 0x04
    unsigned int bitsPerPixel;      // 0x14
    unsigned int compareBits[2];    // 0x18
};
static_assert(sizeof(PixelFormat) == 0x20, "NI::PixelFormat failed size validation");

struct PixelData : Object {
    PixelFormat pixelFormat;       // 0x08
    void* palette;                 // 0x28
    unsigned char* pixels;         // 0x2C
    unsigned int* widths;          // 0x30
    unsigned int* heights;         // 0x34
    unsigned int* offsets;         // 0x38
    unsigned int mipMapLevels;     // 0x3C
    unsigned int bytesPerPixel;    // 0x40
    unsigned int revisionID;       // 0x44
};
static_assert(sizeof(PixelData) == 0x48, "NI::PixelData failed size validation");
static_assert(offsetof(PixelData, pixelFormat) == 0x08, "NI::PixelData::pixelFormat failed offset validation");
static_assert(offsetof(PixelData, mipMapLevels) == 0x3C, "NI::PixelData::mipMapLevels failed offset validation");

struct Texture : ObjectNET {
    // NI::Texture::FormatPrefs
    unsigned int pixelLayout;   // 0x14
    unsigned int mipMapped;     // 0x18
    unsigned int alphaFormat;   // 0x1C
    void* rendererData;         // 0x20
    Texture* previousTexture;   // 0x24
    Texture* nextTexture;       // 0x28
};
static_assert(sizeof(Texture) == 0x2C, "NI::Texture failed size validation");

struct SourceTexture : Texture {
    const char* fileName;            // 0x2C
    const char* platformFileName;    // 0x30
    Pointer<PixelData> pixelData;    // 0x34
    bool isStatic;                   // 0x38
    char pad_39[0x3];                // 0x39
};
static_assert(sizeof(SourceTexture) == 0x3C, "NI::SourceTexture failed size validation");
static_assert(offsetof(SourceTexture, pixelData) == 0x34, "NI::SourceTexture::pixelData failed offset validation");

struct File;

// MWSE declares NI::File but not its vtable; these five slots come from MGE's
// own use of the DDS reader path.
struct FileVtbl {
    void* deletingDtor;                                     // 0x00
    void* asBool;                                           // 0x04
    unsigned int(__thiscall* read)(File*, void*, unsigned int);  // 0x08
    void* write;                                            // 0x0C
    void(__thiscall* seek)(File*, int, int);                // 0x10
};

struct File {
    FileVtbl* vtbl;                   // 0x00
    void* buffer;                     // 0x04
    unsigned int bufferAllocSize;     // 0x08
    unsigned int bufferReadSize;      // 0x0C
    unsigned int position;            // 0x10
    void* filePointer;                // 0x14
    int accessMode;                   // 0x18
    bool valid;                       // 0x1C
    char pad_1D[0x3];                 // 0x1D
};
static_assert(sizeof(File) == 0x20, "NI::File failed size validation");
static_assert(offsetof(File, position) == 0x10, "NI::File::position failed offset validation");

// No MWSE equivalent; MWSE knows only the vtable address 0x751320.
struct DDSReader {
    void* vtbl;                    // 0x00
    unsigned int width;            // 0x04
    unsigned int height;           // 0x08
    unsigned int mipMapLevels;     // 0x0C
    PixelFormat pixelFormat;       // 0x10
};
static_assert(sizeof(DDSReader) == 0x30, "NI::DDSReader failed size validation");
static_assert(offsetof(DDSReader, pixelFormat) == 0x10, "NI::DDSReader::pixelFormat failed offset validation");

// No MWSE equivalent.
struct DX8RendererTextureData {
    void* vtbl;                        // 0x00
    void* unknown_0x4;                 // 0x04
    SourceTexture* sourceTexture;      // 0x08
    void* unknown_0xC;                 // 0x0C
    DX8Renderer* renderer;             // 0x10
    int pixelFormat[10];               // 0x14
    void* d3dPalette;                  // 0x3C
    int d3dPaletteRevision;            // 0x40
    unsigned int width, height;        // 0x44
    unsigned int levels;               // 0x4C
    bool bMipmap;                      // 0x50
    char pad_51[0x3];                  // 0x51
    int unknown_0x54;                  // 0x54
    void* sourcePalette;               // 0x58
    int sourcePaletteRevision;         // 0x5C
    IDirect3DTexture8* d3dTexture;     // 0x60
    int sourceRevision;                // 0x64
};
static_assert(sizeof(DX8RendererTextureData) == 0x68, "NI::DX8RendererTextureData failed size validation");

}  // namespace NI
