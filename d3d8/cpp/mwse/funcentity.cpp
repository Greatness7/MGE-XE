
#include "mge/mwbridge.h"
#include "funcentity.h"
#include "tes3/nitypes.h"
#include "tes3/tes3types.h"

#include <cstdio>



using namespace TES3;

static BaseObject* findEntity(const char* id) {
    const auto resolveObject = reinterpret_cast<BaseObject* (__thiscall*)(NonDynamicData*, const char*)>(
        Address::NonDynamicData_resolveObject);

    return resolveObject(DataHandler::get()->nonDynamicData, id);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseGetBaseHealth)

// [ref] GetBaseHealth -> returns <float>
bool mwseGetBaseHealth::execute(mwseInstruction* _this) {
    VMFLOAT ret = 0;
    const BYTE* actor = reinterpret_cast<const BYTE*>(vmGetTargetActor());

    if (actor) {
        ret = *reinterpret_cast<const float*>(actor + 0x2b8);
    }

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseGetBaseMagicka)

// [ref] GetBaseMagicka -> returns <float>
bool mwseGetBaseMagicka::execute(mwseInstruction* _this) {
    VMFLOAT ret = 0;
    const BYTE* actor = reinterpret_cast<const BYTE*>(vmGetTargetActor());

    if (actor) {
        ret = *reinterpret_cast<const float*>(actor + 0x2c4);
    }

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseGetBaseFatigue)

// [ref] GetBaseFatigue -> returns <float>
bool mwseGetBaseFatigue::execute(mwseInstruction* _this) {
    VMFLOAT ret = 0;
    const BYTE* actor = reinterpret_cast<const BYTE*>(vmGetTargetActor());

    if (actor) {
        ret = *reinterpret_cast<const float*>(actor + 0x2dc);
    }

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseLastActorHit)

// [ref] LastActorHit -> returns <ref>
bool mwseLastActorHit::execute(mwseInstruction* _this) {
    VMREGTYPE ret = 0;
    const BYTE* actor = reinterpret_cast<const BYTE*>(vmGetTargetActor());

    if (actor) {
        const BYTE* targetActor = *reinterpret_cast<const BYTE* const*>(actor + 0xf0);
        if (targetActor) {
            ret = *reinterpret_cast<const VMREGTYPE*>(targetActor + 0x14);
        }
    }

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseGetDeleted)

// [ref] GetDeleted -> returns <long>
bool mwseGetDeleted::execute(mwseInstruction* _this) {
    const Reference* refr = vmGetTargetRef();
    VMREGTYPE ret = (refr->objectFlags & ObjectFlag::Delete) != 0;
    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseIsScripted)

// [ref] IsScripted -> returns <long>
bool mwseIsScripted::execute(mwseInstruction* _this) {
    const Reference* refr = vmGetTargetRef();
    const BaseObject* entity = refr->baseObject;

    // Call <vtbl + 0x4c> const char * Entity::getScript
    typedef const char* (__thiscall *getScript_t)(const BaseObject*);
    getScript_t getScript = reinterpret_cast<getScript_t>(entity->vTable[0x13]);
    VMREGTYPE ret = getScript(entity) != 0;

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseSetEntityName)

// Work around MWSE's SetEntityName bug.
// [ref] SetEntityName <string name>
bool mwseSetEntityName::execute(mwseInstruction* _this) {
    Reference* refr = vmGetTargetRef();
    BaseObject* entity = refr->baseObject;
    const char* name = _this->vmPopString();
    if (!name) { return false; }

    // Check if entity is an actor clone, as only the base entity can set the name
    typedef char (__thiscall *isClone_t)(const BaseObject*);
    isClone_t isClone = reinterpret_cast<isClone_t>(Address::BaseObject_isClone);
    if (isClone(entity)) {
        // Get base entity via NPCInstance::baseNPC
        entity = reinterpret_cast<NPCInstance*>(entity)->baseNPC;
    }

    // Although all entities have a setName virtual function, a few do not implement it.
    // This switch covers the extra cases.
    size_t nameOffset = 0;
    switch (entity->objectType) {
    case MWTag_Apparatus:
        nameOffset = 0x64;
        break;
    case MWTag_Door:
        nameOffset = 0x34;
        break;
    case MWTag_Ingredient:
    case MWTag_Lockpick:
    case MWTag_Probe:
    case MWTag_RepairItem:
        nameOffset = 0x44;
        break;
    }

    if (nameOffset) {
        std::snprintf(reinterpret_cast<char*>(entity) + nameOffset, 32, "%s", name);
    } else {
        // Call <vtbl + 0x10c> Entity::setName(const char *)
        typedef void (__thiscall *setName_t)(BaseObject*, const char*);
        setName_t setName = reinterpret_cast<setName_t>(entity->vTable[0x43]);
        setName(entity, name);
    }

    return true;
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseSetOwner)

// Provide the missing SetOwner opcode.
// [ref] SetOwner <string id> -> <long success>
bool mwseSetOwner::execute(mwseInstruction* _this) {
    VMREGTYPE ret = 0;
    Reference* refr = vmGetTargetRef();
    const char* id = _this->vmPopString();
    if (!id) { return false; }

    typedef DWORD * (__thiscall *getConditionData_t)(Reference*);
    const getConditionData_t getConditionData = reinterpret_cast<getConditionData_t>(
        Address::Reference_getAttachment6ItemData);
    DWORD* cond = getConditionData(refr);

    if (cond) {
        // Check if owner is a base NPC
        const BaseObject* owner = findEntity(id);
        if (owner && owner->vTable == reinterpret_cast<void**>(Address::vtable_NPCBase)) {
            // Write owner into item condition struct
            cond[1] = reinterpret_cast<DWORD>(owner);
            // Set reference modified flag
            refr->objectFlags |= ObjectFlag::Modified;
            ret = 1;
        }
    }

    return _this->vmPush(ret);
}


MWSEINSTRUCTION_DECLARE_VTABLE(mwseModelSwitchNode)

// [ref] mwseModelSwitchNode <string node_name> <long switch_index>
bool mwseModelSwitchNode::execute(mwseInstruction* _this) {
    VMREGTYPE index = -1;
    Reference* refr = vmGetTargetRef();
    const char* node_name = _this->vmPopString();
    if (!node_name) { return false; }
    if (!_this->vmPop(&index)) { return false; }

    if (refr->sceneNode) {
        NI::Node* node = refr->sceneNode;

        // Call <vtbl + 0x5c> AVObject * AVObject::getObjectByName(const char *)
        typedef NI::Node * (__thiscall *findChildNode_t)(const NI::Node*, const char*);
        const findChildNode_t findChildNode = reinterpret_cast<findChildNode_t>(
            reinterpret_cast<void**>(node->vTable)[0x17]);
        NI::Node* child = findChildNode(node, node_name);

        // Check if child is an NiSwitchNode. NiSwitchNode itself is not declared;
        // its selected index sits at +0xB0.
        if (child && reinterpret_cast<uintptr_t>(child->vTable) == Address::vtable_NiSwitchNode) {
            int* switch_index = reinterpret_cast<int*>(reinterpret_cast<BYTE*>(child) + 0xb0);
            *switch_index = index;
        }
    }

    return true;
}
