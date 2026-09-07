
#include "funcphysics.h"
#include "proxydx/d3d8header.h"
#include "support/log.h"
#include "tes3/nitypes.h"
#include "tes3/tes3addresses.h"
#include "tes3/tes3types.h"



using namespace TES3;

static NI::Pick* pick = reinterpret_cast<NI::Pick*>(Address::global_pick);
static NI::PickRecord lastHit;

typedef bool (__thiscall* pickObjects_t)(NI::Pick*, const float*, const float*, bool, float);
typedef void (__thiscall* pickClearResults_t)(NI::Pick*);
typedef Reference* (__cdecl* findNodeReference_t)(NI::AVObject*);
const pickObjects_t pickObjects = (pickObjects_t)Address::NI_Pick_pickObjects;
const pickClearResults_t pickClearResults = (pickClearResults_t)Address::NI_Pick_clearResults;
const findNodeReference_t findNodeReference = (findNodeReference_t)Address::getAssociatedReference;



static bool invokeRayTest(mwseInstruction* _this, const D3DXVECTOR3* pos, const D3DXVECTOR3* dir) {
    VMFLOAT hit_t = FLT_MAX;
    VMREGTYPE hit = 0;

    Reference* refr = mwseInstruction::vmGetTargetRef();
    if (refr) {
        NI::Node* node = refr->sceneNode;
        // Hide reference's NiNode to avoid self-collision
        if (node && (node->flags & 1) == 0) {
            node->flags ^= 1;
        } else {
            node = 0;
        }

        pick->root = Game::get()->worldRoot;

        float inv_length = 1.0f / D3DXVec3Length(dir);
        D3DXVECTOR3 dir_norm;
        D3DXVec3Scale(&dir_norm, dir, inv_length);
        if (pickObjects(pick, *pos, dir_norm, false, 0.0f)) {
            lastHit = *pick->results.storage[0];
            hit_t = lastHit.distance * inv_length;
            hit = 1;
            /*LOG::logline("      -> from=%.0f,%.0f,%.0f to=%.0f,%.0f,%.0f normal=%.3f,%.3f,%.3f t=%.3f dist=%.1f",
                         pos[0], pos[1], pos[2],
                         lastHit.intersect[0], lastHit.intersect[1], lastHit.intersect[2],
                         lastHit.normal[0], lastHit.normal[1], lastHit.normal[2],
                         hit_t, lastHit.distance);*/

            // Fix hit data for skinned meshes
            if (reinterpret_cast<uintptr_t>(lastHit.object->vTable) == NI::VirtualTable::TriShape
                && lastHit.object->skinInstance) {
                // Use reference origin if possible, as actors consist of several skinned meshes
                Reference* refrHit = findNodeReference(lastHit.object);
                const NI::Point3& origin = refrHit ? refrHit->position : lastHit.object->worldBoundOrigin;

                lastHit.intersection.x = pos->x + hit_t * dir->x;
                lastHit.intersection.y = pos->y + hit_t * dir->y;
                lastHit.intersection.z = pos->z + hit_t * dir->z;

                // Use cylindrical bounds to calculate normal
                D3DXVECTOR3 radial;
                radial[0] = lastHit.intersection.x - origin.x;
                radial[1] = lastHit.intersection.y - origin.y;
                radial[2] = 1e-5;
                D3DXVec3Normalize(reinterpret_cast<D3DXVECTOR3*>(&lastHit.normal), &radial);
                /*LOG::logline("      => to=%.0f,%.0f,%.0f normal=%.3f,%.3f,%.3f",
                             lastHit.intersect[0], lastHit.intersect[1], lastHit.intersect[2],
                             lastHit.normal[0], lastHit.normal[1], lastHit.normal[2]);*/
            }
        }

        pickClearResults(pick);
        pick->root = nullptr;

        // Restore visibility of reference's NiNode
        if (node) {
            node->flags ^= 1;
        }
    }

    _this->vmPush(hit_t);
    _this->vmPush(hit);
    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseRayTest)

// [ref] RayTest <float dir.x> <float dir.y> <float dir.z> -> <long hit> <float hit_t>
bool mwseRayTest::execute(mwseInstruction* _this) {
    D3DXVECTOR3 pos, dir;

    if (!_this->vmPop(&dir.x)) { return false; }
    if (!_this->vmPop(&dir.y)) { return false; }
    if (!_this->vmPop(&dir.z)) { return false; }

    Reference* refr = mwseInstruction::vmGetTargetRef();
    if (refr) {
        pos = D3DXVECTOR3(&refr->position.x);
        return invokeRayTest(_this, &pos, &dir);
    }

    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseRayTestFrom)

// [ref] RayTestFrom <float pos.x> <float pos.y> <float pos.z> <float dir.x> <float dir.y> <float dir.z> -> <long hit> <float hit_t>
bool mwseRayTestFrom::execute(mwseInstruction* _this) {
    D3DXVECTOR3 pos, dir;

    if (!_this->vmPop(&pos.x)) { return false; }
    if (!_this->vmPop(&pos.y)) { return false; }
    if (!_this->vmPop(&pos.z)) { return false; }
    if (!_this->vmPop(&dir.x)) { return false; }
    if (!_this->vmPop(&dir.y)) { return false; }
    if (!_this->vmPop(&dir.z)) { return false; }

    return invokeRayTest(_this, &pos, &dir);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseRayHitPosition)

// RayHitPosition -> <float x> <float y> <float z>
bool mwseRayHitPosition::execute(mwseInstruction* _this) {
    _this->vmPush(static_cast<VMFLOAT>(lastHit.intersection.z));
    _this->vmPush(static_cast<VMFLOAT>(lastHit.intersection.y));
    _this->vmPush(static_cast<VMFLOAT>(lastHit.intersection.x));
    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseRayHitNormal)

// RayHitNormal -> <float nx> <float ny> <float nz>
bool mwseRayHitNormal::execute(mwseInstruction* _this) {
    _this->vmPush(static_cast<VMFLOAT>(lastHit.normal.z));
    _this->vmPush(static_cast<VMFLOAT>(lastHit.normal.y));
    _this->vmPush(static_cast<VMFLOAT>(lastHit.normal.x));
    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseRayHitRef)

// RayHitRef -> <ref r>
bool mwseRayHitRef::execute(mwseInstruction* _this) {
    Reference* refrHit = findNodeReference(lastHit.object);
    return _this->vmPush(reinterpret_cast<VMREGTYPE>(refrHit));
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseModelBounds)

// [ref] ModelBounds -> <float min.x> <float min.y> <float min.z> <float max.x> <float max.y> <float max.z>
bool mwseModelBounds::execute(mwseInstruction* _this) {
    Reference* refr = vmGetTargetRef();
    PhysicalObject* entity = refr->baseObject;

    if (entity->boundingBox) {
        _this->vmPush(entity->boundingBox->maximum.z);
        _this->vmPush(entity->boundingBox->maximum.y);
        _this->vmPush(entity->boundingBox->maximum.x);
        _this->vmPush(entity->boundingBox->minimum.z);
        _this->vmPush(entity->boundingBox->minimum.y);
        _this->vmPush(entity->boundingBox->minimum.x);
    } else {
        for (size_t i = 0; i < 6; ++i) {
            _this->vmPush(VMFLOAT(0));
        }
    }
    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseTransformVec)

// [ref] TransformVec <float v.x> <float v.y> <float v.z> -> <float v.x> <float v.y> <float v.z>
// Transform a vector by the reference's local transform
bool mwseTransformVec::execute(mwseInstruction* _this) {
    Reference* refr = vmGetTargetRef();
    NI::Node* node = refr->sceneNode;
    D3DXVECTOR3 v, t;

    if (!_this->vmPop(&v.x)) { return false; }
    if (!_this->vmPop(&v.y)) { return false; }
    if (!_this->vmPop(&v.z)) { return false; }

    const float* m = &node->localRotation->m0.x;
    D3DXVec3Scale(&v, &v, node->localScale);
    t.x = m[0]*v.x + m[1]*v.y + m[2]*v.z + node->localTranslate.x;
    t.y = m[3]*v.x + m[4]*v.y + m[5]*v.z + node->localTranslate.y;
    t.z = m[6]*v.x + m[7]*v.y + m[8]*v.z + node->localTranslate.z;

    _this->vmPush(t.z);
    _this->vmPush(t.y);
    _this->vmPush(t.x);
    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseIsAirborne)

// [ref] IsAirborne -> <long>
bool mwseIsAirborne::execute(mwseInstruction* _this) {
    void* actor = vmGetTargetActor();
    VMREGTYPE ret = 0;

    if (actor) {
        ret = (reinterpret_cast<BYTE*>(actor)[0x10] & 0x10) != 0;
    }

    return _this->vmPush(ret);
}

MWSEINSTRUCTION_DECLARE_VTABLE(mwseSetAirVelocity)

// [ref] SetAirVelocity <float v.x> <float v.y> <float v.z>
bool mwseSetAirVelocity::execute(mwseInstruction* _this) {
    void* actor = vmGetTargetActor();
    D3DXVECTOR3 v;

    if (!_this->vmPop(&v.x)) { return false; }
    if (!_this->vmPop(&v.y)) { return false; }
    if (!_this->vmPop(&v.z)) { return false; }

    if (actor) {
        *reinterpret_cast<D3DXVECTOR3*>(reinterpret_cast<BYTE*>(actor) + 0x3c) = v;
    }

    return true;
}
