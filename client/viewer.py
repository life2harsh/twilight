import io
import os

os.environ.setdefault("PYGAME_HIDE_SUPPORT_PROMPT", "1")
import pygame

import protocol as p

SPECIAL = {
    pygame.K_BACKSPACE: 0xFF08,
    pygame.K_TAB: 0xFF09,
    pygame.K_RETURN: 0xFF0D,
    pygame.K_KP_ENTER: 0xFF8D,
    pygame.K_ESCAPE: 0xFF1B,
    pygame.K_HOME: 0xFF50,
    pygame.K_LEFT: 0xFF51,
    pygame.K_UP: 0xFF52,
    pygame.K_RIGHT: 0xFF53,
    pygame.K_DOWN: 0xFF54,
    pygame.K_PAGEUP: 0xFF55,
    pygame.K_PAGEDOWN: 0xFF56,
    pygame.K_END: 0xFF57,
    pygame.K_LSHIFT: 0xFFE1,
    pygame.K_RSHIFT: 0xFFE2,
    pygame.K_LCTRL: 0xFFE3,
    pygame.K_RCTRL: 0xFFE4,
    pygame.K_LALT: 0xFFE9,
    pygame.K_RALT: 0xFFEA,
    pygame.K_LGUI: 0xFFEB,
    pygame.K_RGUI: 0xFFEC,
    pygame.K_DELETE: 0xFFFF,
    **{getattr(pygame, f"K_F{n}"): 0xFFBD + n for n in range(1, 13)},
}


def typed(text):
    if len(text) == 1 and text.isprintable():
        code = ord(text)
        return code if code < 0x100 else 0x01000000 + code
    return None


def keysym(key):
    if key in SPECIAL:
        return SPECIAL[key]
    if 0x20 <= key <= 0x7E or 0xA0 <= key <= 0xFF:
        return key
    if 0x100 <= key < 0x40000000:
        return 0x01000000 + key
    return None


class Viewer:
    def __init__(self, send, width, height):
        pygame.init()
        desktop = pygame.display.get_desktop_sizes()[0]
        scale = min(1, desktop[0] * 0.9 / width, desktop[1] * 0.9 / height)
        self.screen = pygame.display.set_mode((int(width * scale), int(height * scale)), pygame.RESIZABLE)
        pygame.display.set_caption("Twilight")
        self.font = pygame.font.Font(None, 22)
        self.send = send
        self.area = None
        self.held = {}

    def to_remote(self, pos):
        if not self.area or not self.area.collidepoint(pos):
            return None
        x = (pos[0] - self.area.x) * 65535 // max(self.area.w - 1, 1)
        y = (pos[1] - self.area.y) * 65535 // max(self.area.h - 1, 1)
        return x, y

    def pump(self):
        move = None
        for event in pygame.event.get():
            if event.type == pygame.QUIT:
                return False
            if not self.send:
                continue
            if event.type == pygame.MOUSEMOTION:
                move = self.to_remote(event.pos)
                continue
            if move:
                self.send(p.MOUSE_MOVE, x=move[0], y=move[1])
                move = None
            if event.type in (pygame.MOUSEBUTTONDOWN, pygame.MOUSEBUTTONUP) and event.button in (1, 2, 3):
                self.send(p.MOUSE_BUTTON, state=int(event.type == pygame.MOUSEBUTTONDOWN), code=event.button)
            elif event.type == pygame.MOUSEWHEEL:
                self.send(p.SCROLL, x=event.x & 0xFFFF, y=-event.y & 0xFFFF)
            elif event.type == pygame.KEYDOWN and (sym := self.held.get(event.key) or typed(event.unicode) or keysym(event.key)):
                self.held[event.key] = sym
                self.send(p.KEY, state=1, code=sym)
            elif event.type == pygame.KEYUP and (sym := self.held.pop(event.key, None) or keysym(event.key)):
                self.send(p.KEY, state=0, code=sym)
        if move:
            self.send(p.MOUSE_MOVE, x=move[0], y=move[1])
        return True

    def show(self, jpeg, lines):
        try:
            image = pygame.image.load(io.BytesIO(jpeg), "frame.jpg")
        except pygame.error:
            return False
        window = self.screen.get_size()
        width, height = image.get_size()
        scale = min(window[0] / width, window[1] / height)
        size = (max(1, int(width * scale)), max(1, int(height * scale)))
        if size != (width, height):
            image = pygame.transform.scale(image, size)
        self.area = image.get_rect(center=self.screen.get_rect().center)
        self.screen.fill((0, 0, 0))
        self.screen.blit(image, self.area)
        for row, text in enumerate(lines):
            self.screen.blit(self.font.render(text, True, (80, 255, 120), (0, 0, 0)), (8, 8 + row * 20))
        pygame.display.flip()
        return True

    def close(self):
        pygame.quit()
