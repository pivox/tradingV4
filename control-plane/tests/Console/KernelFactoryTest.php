<?php

declare(strict_types=1);

namespace App\Tests\Console;

use App\Console\KernelFactory;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;
use Symfony\Component\Console\Input\ArgvInput;

final class KernelFactoryTest extends TestCase
{
    /**
     * @return iterable<string, array{list<string>, array<string, string>, string}>
     */
    public static function environments(): iterable
    {
        yield 'défaut prod' => [['console', 'tv4:report'], [], 'prod'];
        yield 'APP_ENV' => [['console', 'tv4:report'], ['APP_ENV' => 'test'], 'test'];
        yield '--env=' => [['console', 'tv4:report', '--env=dev'], ['APP_ENV' => 'test'], 'dev'];
        yield '--env valeur' => [['console', '--env', 'dev', 'tv4:report'], [], 'dev'];
        yield '-e' => [['console', 'tv4:report', '-e', 'dev'], [], 'dev'];
        yield 'après --' => [['console', 'tv4:report', '--', '--env=dev'], [], 'prod'];
    }

    /**
     * @param list<string>          $argv
     * @param array<string, string> $server
     */
    #[DataProvider('environments')]
    public function testEnvironment(array $argv, array $server, string $expected): void
    {
        self::assertSame($expected, KernelFactory::environment(new ArgvInput($argv), $server));
    }

    /**
     * @return iterable<string, array{list<string>, array<string, string>, bool}>
     */
    public static function debugModes(): iterable
    {
        yield 'APP_DEBUG absent : debug actif' => [['console', 'tv4:report'], [], true];
        yield 'APP_DEBUG=1' => [['console', 'tv4:report'], ['APP_DEBUG' => '1'], true];
        yield 'APP_DEBUG=true' => [['console', 'tv4:report'], ['APP_DEBUG' => 'true'], true];
        yield 'APP_DEBUG=0' => [['console', 'tv4:report'], ['APP_DEBUG' => '0'], false];
        yield 'APP_DEBUG=false' => [['console', 'tv4:report'], ['APP_DEBUG' => 'false'], false];
        yield '--no-debug' => [['console', 'tv4:report', '--no-debug'], [], false];
        yield '--no-debug l\'emporte sur APP_DEBUG=1' => [['console', '--no-debug', 'tv4:report'], ['APP_DEBUG' => '1'], false];
    }

    /**
     * @param list<string>          $argv
     * @param array<string, string> $server
     */
    #[DataProvider('debugModes')]
    public function testDebug(array $argv, array $server, bool $expected): void
    {
        self::assertSame($expected, KernelFactory::debug(new ArgvInput($argv), $server));
    }

    public function testCreateBuildsTheKernelFromInputAndServer(): void
    {
        $kernel = KernelFactory::create(new ArgvInput(['console', '-e', 'test', '--no-debug']), []);

        self::assertSame('test', $kernel->getEnvironment());
        self::assertFalse($kernel->isDebug());
    }
}
