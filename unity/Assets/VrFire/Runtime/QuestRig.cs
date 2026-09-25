// Minimal VR rig for the Quest build: the headset drives the camera (TrackedPoseDriver on the
// child camera, set up by VrFireBatch.BuildQuestScene); the thumbsticks move the rig.
//
// Left stick: move where you're looking (horizontal). Right stick left/right: 30° snap turn.
// Right stick up/down: climb or descend. The rig never goes below the terrain (raycast down
// against the tiles' mesh colliders) and starts standing on the ground at its position.

using UnityEngine;
using UnityEngine.InputSystem;

namespace VrFire
{
    public class QuestRig : MonoBehaviour
    {
        public Transform head;
        public float speed = 12f;        // m/s at full stick
        public float climbSpeed = 20f;   // m/s
        public float snapDegrees = 30f;

        InputAction move, turn;
        bool turnHeld;

        void OnEnable()
        {
            move = new InputAction("Move", InputActionType.Value, "<XRController>{LeftHand}/{Primary2DAxis}");
            turn = new InputAction("Turn", InputActionType.Value, "<XRController>{RightHand}/{Primary2DAxis}");
            move.Enable();
            turn.Enable();
        }

        void OnDisable()
        {
            move?.Disable();
            turn?.Disable();
        }

        void Start()
        {
            if (Ground(transform.position, out float h)) transform.position = new Vector3(transform.position.x, h, transform.position.z);
        }

        static bool Ground(Vector3 p, out float height)
        {
            if (Physics.Raycast(new Vector3(p.x, 20000f, p.z), Vector3.down, out var hit, 40000f))
            {
                height = hit.point.y;
                return true;
            }
            height = 0f;
            return false;
        }

        void Update()
        {
            float dt = Time.deltaTime;
            Vector2 m = move.ReadValue<Vector2>();
            Vector2 r = turn.ReadValue<Vector2>();

            // Move in the head's horizontal facing.
            Vector3 fwd = head != null ? head.forward : transform.forward;
            fwd.y = 0f;
            fwd = fwd.sqrMagnitude > 1e-4f ? fwd.normalized : transform.forward;
            Vector3 right = Vector3.Cross(Vector3.up, fwd);
            Vector3 delta = (fwd * m.y + right * m.x) * speed * dt;

            // Snap turn around the head, not the rig origin, so the view doesn't slide.
            if (Mathf.Abs(r.x) > 0.7f && !turnHeld)
            {
                turnHeld = true;
                Vector3 pivot = head != null ? head.position : transform.position;
                transform.RotateAround(pivot, Vector3.up, Mathf.Sign(r.x) * snapDegrees);
            }
            else if (Mathf.Abs(r.x) < 0.3f)
            {
                turnHeld = false;
            }

            Vector3 p = transform.position + delta;
            p.y += (Mathf.Abs(r.y) > 0.2f ? r.y : 0f) * climbSpeed * dt;
            if (Ground(p, out float h) && p.y < h) p.y = h;
            transform.position = p;
        }
    }
}
