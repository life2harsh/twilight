import io
import os

os.environ["SDL_VIDEODRIVER"] = "dummy"
import pygame

import protocol as p
from viewer import Viewer, keysym


def jpeg(width, height):
    surface = pygame.Surface((width, height))
    surface.fill((200, 10, 10))
    out = io.BytesIO()
    pygame.image.save(surface, out, "frame.jpg")
    return out.getvalue()


def test_keysyms():
    assert keysym(pygame.K_a) == ord("a")
    assert keysym(pygame.K_RETURN) == 0xFF0D
    assert keysym(pygame.K_F1) == 0xFFBE and keysym(pygame.K_F12) == 0xFFC9
    assert keysym(pygame.K_LCTRL) == 0xFFE3


def test_events_become_input():
    sent = []
    viewer = Viewer(lambda kind, **fields: sent.append((kind, fields)), 320, 180)
    assert viewer.show(jpeg(320, 180), ["UDP"])
    pygame.event.post(pygame.event.Event(pygame.MOUSEMOTION, pos=(0, 0), rel=(0, 0), buttons=(0, 0, 0)))
    pygame.event.post(pygame.event.Event(pygame.MOUSEBUTTONDOWN, pos=(0, 0), button=1))
    pygame.event.post(pygame.event.Event(pygame.MOUSEMOTION, pos=(319, 179), rel=(0, 0), buttons=(0, 0, 0)))
    pygame.event.post(pygame.event.Event(pygame.MOUSEWHEEL, x=0, y=1, flipped=False))
    pygame.event.post(pygame.event.Event(pygame.KEYDOWN, key=pygame.K_a, mod=0, unicode="a", scancode=4))
    pygame.event.post(pygame.event.Event(pygame.KEYUP, key=pygame.K_a, mod=0, unicode="a", scancode=4))
    pygame.event.post(pygame.event.Event(pygame.KEYDOWN, key=pygame.K_j, mod=pygame.KMOD_SHIFT, unicode="J", scancode=13))
    pygame.event.post(pygame.event.Event(pygame.KEYUP, key=pygame.K_j, mod=pygame.KMOD_SHIFT, unicode="", scancode=13))
    pygame.event.post(pygame.event.Event(pygame.KEYDOWN, key=pygame.K_c, mod=pygame.KMOD_CTRL, unicode="\x03", scancode=6))
    assert viewer.pump()
    viewer.close()
    assert sent == [
        (p.MOUSE_MOVE, {"x": 0, "y": 0}),
        (p.MOUSE_BUTTON, {"state": 1, "code": 1}),
        (p.MOUSE_MOVE, {"x": 65535, "y": 65535}),
        (p.SCROLL, {"x": 0, "y": 0xFFFF}),
        (p.KEY, {"state": 1, "code": ord("a")}),
        (p.KEY, {"state": 0, "code": ord("a")}),
        (p.KEY, {"state": 1, "code": ord("J")}),
        (p.KEY, {"state": 0, "code": ord("J")}),
        (p.KEY, {"state": 1, "code": ord("c")}),
    ]
