<?php

declare(strict_types=1);

namespace App\Tests\Console;

use App\Console\Tv4Application;
use App\Tests\Support\RunFixture;
use Symfony\Bundle\FrameworkBundle\Test\KernelTestCase;
use Symfony\Component\Console\Input\ArrayInput;
use Symfony\Component\Console\Output\BufferedOutput;

/** Codes de sortie de `bin/console` : 0 succès, 1 run invalide, 2 erreur d'usage. */
final class Tv4ApplicationTest extends KernelTestCase
{
    private ?RunFixture $copy = null;

    protected function tearDown(): void
    {
        $this->copy?->cleanup();
        $this->copy = null;
        parent::tearDown();
    }

    /**
     * @param array<string, mixed> $parameters
     *
     * @return array{int, string}
     */
    private function runConsole(array $parameters): array
    {
        $application = new Tv4Application(self::bootKernel());
        $application->setAutoExit(false);
        $output = new BufferedOutput();
        $code = $application->run(new ArrayInput($parameters), $output);

        return [$code, $output->fetch()];
    }

    public function testSuccessExitsWithZero(): void
    {
        $this->copy = RunFixture::copy();

        [$code, $display] = $this->runConsole([
            'command' => 'tv4:report',
            'run_dir' => $this->copy->directory,
            '--output' => $this->copy->path('rapport.html'),
        ]);

        self::assertSame(0, $code, $display);
        self::assertFileExists($this->copy->path('rapport.html'));
    }

    public function testInvalidRunExitsWithOne(): void
    {
        $this->copy = RunFixture::copy();
        $this->copy->remove('trades.csv');

        [$code, $display] = $this->runConsole([
            'command' => 'tv4:report',
            'run_dir' => $this->copy->directory,
            '--output' => $this->copy->path('rapport.html'),
        ]);

        self::assertSame(1, $code);
        self::assertStringContainsString('trades.csv : fichier absent', $display);
    }

    public function testMissingArgumentExitsWithTwo(): void
    {
        [$code, $display] = $this->runConsole(['command' => 'tv4:report']);

        self::assertSame(2, $code);
        self::assertStringContainsString('run_dir', $display);
    }

    public function testUnknownOptionExitsWithTwo(): void
    {
        [$code, $display] = $this->runConsole(['command' => 'tv4:report', 'run_dir' => '.', '--inconnue' => true]);

        self::assertSame(2, $code);
        self::assertStringContainsString('--inconnue', $display);
    }

    public function testUnknownCommandExitsWithTwo(): void
    {
        [$code, $display] = $this->runConsole(['command' => 'tv4:inconnue']);

        self::assertSame(2, $code);
        self::assertStringContainsString('tv4:inconnue', $display);
    }
}
