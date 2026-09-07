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

// `minwindef.h` defines `near` and `far` as empty macros for 16-bit source
// compatibility, which would silently delete the `Frustum` members of the same
// name. Both expand to nothing, so dropping them costs nothing.
#undef near
#undef far

struct IDirect3DDevice8;

namespace NI {

//-----------------------------------------------------------------------------
// Support types
//-----------------------------------------------------------------------------

// NiPointer<T>. A smart pointer in the engine; MGE only ever reads through it,
// so refcounting is deliberately not modelled.
template <typename T>
struct Pointer {
    T* ptr;

    operator T* () const { return ptr; }
    T* operator->() const { return ptr; }
};
static_assert(sizeof(Pointer<int>) == 0x4, "NI::Pointer failed size validation");

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

struct Object {
    void* vTable;    // 0x00
    int refCount;    // 0x04
};
static_assert(sizeof(Object) == 0x8, "NI::Object failed size validation");

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
    Pointer<Object> skinInstance;        // 0x9C
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

//-----------------------------------------------------------------------------
// Camera
//-----------------------------------------------------------------------------

struct Frustum {
    float left;    // 0x00
    float right;   // 0x04
    float top;     // 0x08
    float bottom;  // 0x0C
    float near;    // 0x10
    float far;     // 0x14
};
static_assert(sizeof(Frustum) == 0x18, "NI::Frustum failed size validation");
static_assert(offsetof(Frustum, far) == 0x14, "NI::Frustum::far failed offset validation");

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

// NiDX8Renderer. `Renderer` is the abstract base MWSE declares separately; MGE
// only ever holds the concrete DX8 one, so the base is folded in here.
struct Renderer : Object {
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
static_assert(sizeof(Renderer) == 0x6A0, "NI::Renderer failed size validation");
static_assert(offsetof(Renderer, d3dDevice) == 0x24, "NI::Renderer::d3dDevice failed offset validation");
static_assert(offsetof(Renderer, backbufferRenderTarget) == 0x520, "NI::Renderer::backbufferRenderTarget failed offset validation");
static_assert(offsetof(Renderer, currentRenderTarget) == 0x544, "NI::Renderer::currentRenderTarget failed offset validation");

//-----------------------------------------------------------------------------
// Virtual table addresses
//-----------------------------------------------------------------------------

namespace VirtualTable {
    inline constexpr uintptr_t MaterialProperty = 0x75036C;
}

}  // namespace NI
